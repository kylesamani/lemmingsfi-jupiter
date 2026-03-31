pub mod quote;
pub mod state;

use jupiter_amm_interface::{
    AccountProvider, Amm, AmmContext, AmmError, KeyedAccount, Quote, QuoteParams,
    SwapAndAccountMetas, SwapParams,
};
use solana_account::ReadableAccount;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

pub use quote::{compute_swap_output, QuoteError, QuoteInput, QuoteResult, SwapDirection};
pub use state::{deserialize_market, DeserializeError, GlobalConfigState, MarketState};

/// LemmingsFi program ID.
pub const PROGRAM_ID: Pubkey = solana_pubkey::pubkey!("BQEJZUB4CzoT6UhRffoCkqCyqQNrCPCSGHcPEmsdbEsX");

/// SPL Token program ID.
const TOKEN_PROGRAM_ID: Pubkey = solana_pubkey::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// Instructions sysvar ID.
const SYSVAR_INSTRUCTIONS_ID: Pubkey = solana_pubkey::pubkey!("Sysvar1nstructions1111111111111111111111111");

/// Anchor instruction discriminator: sha256("global:<name>")[..8]
fn anchor_discriminator(name: &str) -> [u8; 8] {
    match name {
        "swap" => [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8],
        _ => panic!("Unknown instruction: {}", name),
    }
}

/// Jupiter AMM implementation for LemmingsFi.
#[derive(Clone)]
pub struct LemmingsFiAmm {
    /// Market PDA address.
    key: Pubkey,
    /// GlobalConfig PDA address.
    global_config: Pubkey,
    /// Deserialized market state.
    market: MarketState,
    /// Base vault balance.
    vault_base_amount: u64,
    /// Quote vault balance.
    vault_quote_amount: u64,
    /// Whether the global kill switch is active.
    global_paused: bool,
}

impl Amm for LemmingsFiAmm {
    fn from_keyed_account(keyed_account: &KeyedAccount, _amm_context: &AmmContext) -> Result<Self, AmmError> {
        let market = deserialize_market(keyed_account.account.data())
            .map_err(|e| AmmError::from(e.to_string()))?;
        let (global_config, _) = state::pda::derive_global_config(&PROGRAM_ID);

        Ok(Self {
            key: keyed_account.key,
            global_config,
            market,
            vault_base_amount: 0,
            vault_quote_amount: 0,
            global_paused: false,
        })
    }

    fn label(&self) -> &'static str {
        "LemmingsFi"
    }

    fn program_id(&self) -> Pubkey {
        PROGRAM_ID
    }

    fn key(&self) -> Pubkey {
        self.key
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        vec![self.market.base_mint, self.market.quote_mint]
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![self.key, self.market.vault_base, self.market.vault_quote, self.global_config]
    }

    fn update(&mut self, account_provider: impl AccountProvider) -> Result<(), AmmError> {
        if let Some(market_account) = account_provider.get(&self.key) {
            self.market = deserialize_market(market_account.data())
                .map_err(|e| AmmError::from(e.to_string()))?;
        }
        if let Some(vault_base) = account_provider.get(&self.market.vault_base) {
            self.vault_base_amount = state::parse_token_amount(vault_base.data())
                .map_err(|e| AmmError::from(e.to_string()))?;
        }
        if let Some(vault_quote) = account_provider.get(&self.market.vault_quote) {
            self.vault_quote_amount = state::parse_token_amount(vault_quote.data())
                .map_err(|e| AmmError::from(e.to_string()))?;
        }
        if let Some(gc_account) = account_provider.get(&self.global_config) {
            let gc = state::deserialize_global_config(gc_account.data())
                .map_err(|e| AmmError::from(e.to_string()))?;
            self.global_paused = gc.paused;
        }
        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote, AmmError> {
        let direction = if quote_params.input_mint == self.market.quote_mint {
            SwapDirection::BuyBase
        } else {
            SwapDirection::SellBase
        };

        let input = QuoteInput::from(&self.market);
        let result = compute_swap_output(&input, direction, quote_params.amount)
            .map_err(|e| AmmError::from(e.to_string()))?;

        // Cap output at available vault liquidity (matches on-chain InsufficientLiquidity check)
        let out_amount = match direction {
            SwapDirection::BuyBase => result.amount_out.min(self.vault_base_amount),
            SwapDirection::SellBase => result.amount_out.min(self.vault_quote_amount),
        };

        if out_amount == 0 {
            return Err(AmmError::from("Insufficient vault liquidity".to_string()));
        }

        let fee_pct = rust_decimal::Decimal::from(self.market.fee_bps)
            / rust_decimal::Decimal::from(quote::BPS_DENOMINATOR);

        // Compute fee as difference between no-fee and with-fee output
        let fee_amount = {
            let no_fee_input = QuoteInput {
                fee_bps: 0,
                ..input
            };
            let no_fee_result = compute_swap_output(&no_fee_input, direction, quote_params.amount)
                .map_err(|e| AmmError::from(e.to_string()))?;
            no_fee_result.amount_out.saturating_sub(result.amount_out)
        };

        let fee_mint = if direction == SwapDirection::BuyBase {
            self.market.base_mint
        } else {
            self.market.quote_mint
        };

        Ok(Quote {
            in_amount: quote_params.amount,
            out_amount,
            fee_amount,
            fee_mint,
            fee_pct,
        })
    }

    fn get_swap_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> Result<SwapAndAccountMetas, AmmError> {
        let (direction, user_base, user_quote) = if swap_params.source_mint == self.market.quote_mint {
            (SwapDirection::BuyBase, swap_params.destination_token_account, swap_params.source_token_account)
        } else {
            (SwapDirection::SellBase, swap_params.source_token_account, swap_params.destination_token_account)
        };

        let mut data = anchor_discriminator("swap").to_vec();
        let direction_byte: u8 = match direction {
            SwapDirection::BuyBase => 0,
            SwapDirection::SellBase => 1,
        };
        data.push(direction_byte);
        data.extend_from_slice(&swap_params.in_amount.to_le_bytes());
        data.extend_from_slice(&swap_params.out_amount.to_le_bytes());

        let accounts = vec![
            AccountMeta::new_readonly(swap_params.token_transfer_authority, true),
            AccountMeta::new_readonly(self.global_config, false),
            AccountMeta::new(self.key, false),
            AccountMeta::new(self.market.vault_base, false),
            AccountMeta::new(self.market.vault_quote, false),
            AccountMeta::new(user_base, false),
            AccountMeta::new(user_quote, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSVAR_INSTRUCTIONS_ID, false),
        ];

        Ok(SwapAndAccountMetas {
            swap: jupiter_amm_interface::Swap::TokenSwap,
            account_metas: accounts,
        })
    }

    fn supports_exact_out(&self) -> bool {
        false
    }

    fn has_dynamic_accounts(&self) -> bool {
        false
    }

    fn unidirectional(&self) -> bool {
        false
    }

    fn program_dependencies(&self) -> Vec<(Pubkey, String)> {
        vec![]
    }

    fn get_accounts_len(&self) -> usize {
        // Market account size: 8 (discriminator) + 234 (data) = 242
        242
    }

    fn is_active(&self) -> bool {
        !self.market.paused && !self.global_paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;
    use solana_account::Account;
    use solana_clock::Clock;
    use std::collections::HashMap;

    const USDC_MINT: Pubkey = solana_pubkey::pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    const USDT_MINT: Pubkey = solana_pubkey::pubkey!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");

    fn test_market_state() -> MarketState {
        let (vault_base, _) = state::pda::derive_vault_base(&PROGRAM_ID, &Pubkey::default());
        let (vault_quote, _) = state::pda::derive_vault_quote(&PROGRAM_ID, &Pubkey::default());
        MarketState {
            base_mint: USDC_MINT,
            quote_mint: USDT_MINT,
            vault_base,
            vault_quote,
            authority: Pubkey::default(),
            oracle_price: 1_000_000, // $1.00
            oracle_conf: 100,
            oracle_timestamp: 0,
            oracle_slot: 0,
            bid_spread_bps: 3,
            ask_spread_bps: 3,
            fee_bps: 5,
            min_order_size: 1_000_000,
            max_order_size: 250_000_000_000,
            concentration: 10_000,
            max_staleness_slots: 20,
            max_price_deviation_bps: 100,
            paused: false,
            bump: 255,
            oracle_authority: Pubkey::default(),
            min_vault_base_reserve: 0,
            min_vault_quote_reserve: 0,
        }
    }

    fn serialize_market_account(market: &MarketState) -> Vec<u8> {
        // 8-byte Anchor discriminator + Borsh-serialized market
        let mut data = vec![0u8; 8]; // discriminator
        market.serialize(&mut data).unwrap();
        data
    }

    fn make_token_account_data(amount: u64) -> Vec<u8> {
        let mut data = vec![0u8; 165]; // SPL Token account size
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data
    }

    fn serialize_global_config(paused: bool) -> Vec<u8> {
        let gc = GlobalConfigState {
            authority: Pubkey::default(),
            fee_recipient: Pubkey::default(),
            default_fee_bps: 5,
            paused,
            bump: 255,
        };
        let mut data = vec![0u8; 8]; // discriminator
        gc.serialize(&mut data).unwrap();
        data
    }

    fn make_amm_context() -> AmmContext {
        let clock = Clock {
            slot: 100,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 0,
        };
        AmmContext {
            clock_ref: jupiter_amm_interface::ClockRef::from(clock),
        }
    }

    fn make_keyed_account(market: &MarketState) -> KeyedAccount {
        let data = serialize_market_account(market);
        KeyedAccount {
            key: Pubkey::default(),
            account: Account {
                lamports: 1_000_000,
                data,
                owner: PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
            params: None,
        }
    }

    // HashMap<Pubkey, Account> already implements AccountProvider via the blanket impl
    // in jupiter-amm-interface. We just need Account to deref to something ReadableAccount.
    // Since Account doesn't impl Deref, we use a wrapper.
    struct OwnedAccount(Account);

    impl std::ops::Deref for OwnedAccount {
        type Target = Account;
        fn deref(&self) -> &Account {
            &self.0
        }
    }

    type MockProvider = HashMap<Pubkey, OwnedAccount>;

    fn mock_provider() -> MockProvider {
        HashMap::new()
    }

    #[test]
    fn test_from_keyed_account() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();

        let amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        assert_eq!(amm.label(), "LemmingsFi");
        assert_eq!(amm.program_id(), PROGRAM_ID);
        assert_eq!(amm.get_reserve_mints(), vec![USDC_MINT, USDT_MINT]);
        assert!(amm.is_active());
    }

    #[test]
    fn test_is_active_paused_market() {
        let mut market = test_market_state();
        market.paused = true;
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();

        let amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        assert!(!amm.is_active());
    }

    #[test]
    fn test_is_active_global_paused() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let mut amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();

        // Simulate global pause via update
        let gc_data = serialize_global_config(true);
        let mut accounts: MockProvider = mock_provider();
        accounts.insert(amm.global_config, OwnedAccount(Account {
            lamports: 1_000_000,
            data: gc_data,
            owner: PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }));
        amm.update(accounts).unwrap();
        assert!(!amm.is_active());
    }

    #[test]
    fn test_update_reads_vault_balances() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let mut amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        assert_eq!(amm.vault_base_amount, 0);
        assert_eq!(amm.vault_quote_amount, 0);

        let mut accounts: MockProvider = mock_provider();
        accounts.insert(market.vault_base, OwnedAccount(Account {
            lamports: 1_000_000,
            data: make_token_account_data(100_000_000_000), // 100K USDC
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
        }));
        accounts.insert(market.vault_quote, OwnedAccount(Account {
            lamports: 1_000_000,
            data: make_token_account_data(100_000_000_000), // 100K USDT
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
        }));
        amm.update(accounts).unwrap();
        assert_eq!(amm.vault_base_amount, 100_000_000_000);
        assert_eq!(amm.vault_quote_amount, 100_000_000_000);
    }

    #[test]
    fn test_quote_buy_base() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let mut amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        amm.vault_base_amount = 100_000_000_000; // 100K USDC
        amm.vault_quote_amount = 100_000_000_000; // 100K USDT

        // Buy 1000 USDC worth of base with quote
        let quote = amm.quote(&QuoteParams {
            amount: 1_000_000_000, // 1000 USDT in
            input_mint: USDT_MINT,
            output_mint: USDC_MINT,
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            fee_mode: jupiter_amm_interface::FeeMode::Normal,
        }).unwrap();

        assert_eq!(quote.in_amount, 1_000_000_000);
        // With 3 bps ask spread + 5 bps fee, output should be slightly less than input
        assert!(quote.out_amount > 0);
        assert!(quote.out_amount < 1_000_000_000); // less due to spread + fee
        assert!(quote.fee_amount > 0);
        assert_eq!(quote.fee_mint, USDC_MINT); // fee in output token
    }

    #[test]
    fn test_quote_sell_base() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let mut amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        amm.vault_base_amount = 100_000_000_000;
        amm.vault_quote_amount = 100_000_000_000;

        let quote = amm.quote(&QuoteParams {
            amount: 1_000_000_000, // 1000 USDC in
            input_mint: USDC_MINT,
            output_mint: USDT_MINT,
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            fee_mode: jupiter_amm_interface::FeeMode::Normal,
        }).unwrap();

        assert_eq!(quote.in_amount, 1_000_000_000);
        assert!(quote.out_amount > 0);
        assert!(quote.out_amount < 1_000_000_000);
        assert!(quote.fee_amount > 0);
        assert_eq!(quote.fee_mint, USDT_MINT);
    }

    #[test]
    fn test_quote_capped_by_vault_liquidity() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let mut amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        amm.vault_base_amount = 100; // only 100 base tokens in vault
        amm.vault_quote_amount = 100_000_000_000;

        // Try to buy a large amount of base
        let quote = amm.quote(&QuoteParams {
            amount: 1_000_000_000,
            input_mint: USDT_MINT,
            output_mint: USDC_MINT,
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            fee_mode: jupiter_amm_interface::FeeMode::Normal,
        }).unwrap();

        // Output capped at vault balance
        assert_eq!(quote.out_amount, 100);
    }

    #[test]
    fn test_quote_empty_vault_returns_error() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();
        // vault_base_amount = 0 (default)

        let result = amm.quote(&QuoteParams {
            amount: 1_000_000_000,
            input_mint: USDT_MINT,
            output_mint: USDC_MINT,
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            fee_mode: jupiter_amm_interface::FeeMode::Normal,
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_get_swap_and_account_metas_buy_base() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();

        let user = Pubkey::new_unique();
        let user_base_ata = Pubkey::new_unique();
        let user_quote_ata = Pubkey::new_unique();

        let result = amm.get_swap_and_account_metas(&SwapParams {
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            in_amount: 1_000_000_000,
            out_amount: 999_000_000,
            source_mint: USDT_MINT,            // paying quote
            destination_mint: USDC_MINT,       // receiving base
            source_token_account: user_quote_ata,
            destination_token_account: user_base_ata,
            token_transfer_authority: user,
            user,
            payer: user,
            quote_mint_to_referrer: None,
            jupiter_program_id: &Pubkey::default(),
            missing_dynamic_accounts_as_default: false,
        }).unwrap();

        // 9 accounts total (includes sysvar_instructions for CPI guard)
        assert_eq!(result.account_metas.len(), 9);

        // Account ordering: user, global_config, market, vault_base, vault_quote, user_base, user_quote, token_program, sysvar_instructions
        assert_eq!(result.account_metas[0].pubkey, user);
        assert!(result.account_metas[0].is_signer);
        assert_eq!(result.account_metas[5].pubkey, user_base_ata);  // destination = user_base for BuyBase
        assert_eq!(result.account_metas[6].pubkey, user_quote_ata); // source = user_quote for BuyBase
        assert_eq!(result.account_metas[7].pubkey, TOKEN_PROGRAM_ID);
    }

    #[test]
    fn test_get_accounts_to_update_includes_global_config() {
        let market = test_market_state();
        let keyed = make_keyed_account(&market);
        let ctx = make_amm_context();
        let amm = LemmingsFiAmm::from_keyed_account(&keyed, &ctx).unwrap();

        let accounts = amm.get_accounts_to_update();
        assert_eq!(accounts.len(), 4); // market, vault_base, vault_quote, global_config
        assert_eq!(accounts[3], amm.global_config);
    }
}
