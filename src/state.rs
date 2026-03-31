use borsh::{BorshDeserialize, BorshSerialize};
use solana_pubkey::Pubkey;

/// Anchor discriminator size (8 bytes).
const DISCRIMINATOR_LEN: usize = 8;

/// On-chain Market account state.
/// Layout matches `programs/lemmingsfi/src/state/market.rs` exactly.
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct MarketState {
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub vault_base: Pubkey,
    pub vault_quote: Pubkey,
    pub authority: Pubkey,
    pub oracle_price: u64,
    pub oracle_conf: u64,
    pub oracle_timestamp: i64,
    pub oracle_slot: u64,
    pub bid_spread_bps: u16,
    pub ask_spread_bps: u16,
    pub fee_bps: u16,
    pub min_order_size: u64,
    pub max_order_size: u64,
    pub concentration: u64,
    pub max_staleness_slots: u64,
    pub max_price_deviation_bps: u16,
    pub paused: bool,
    pub bump: u8,
    pub oracle_authority: Pubkey,
    pub min_vault_base_reserve: u64,
    pub min_vault_quote_reserve: u64,
}

/// On-chain GlobalConfig account state.
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct GlobalConfigState {
    pub authority: Pubkey,
    pub fee_recipient: Pubkey,
    pub default_fee_bps: u16,
    pub paused: bool,
    pub bump: u8,
}

/// Deserialize a Market account from raw account data (includes 8-byte Anchor discriminator).
pub fn deserialize_market(data: &[u8]) -> Result<MarketState, DeserializeError> {
    if data.len() < DISCRIMINATOR_LEN {
        return Err(DeserializeError::DataTooShort);
    }
    MarketState::try_from_slice(&data[DISCRIMINATOR_LEN..])
        .map_err(|e| DeserializeError::BorshError(e.to_string()))
}

/// Deserialize a GlobalConfig account from raw account data.
pub fn deserialize_global_config(data: &[u8]) -> Result<GlobalConfigState, DeserializeError> {
    if data.len() < DISCRIMINATOR_LEN {
        return Err(DeserializeError::DataTooShort);
    }
    GlobalConfigState::try_from_slice(&data[DISCRIMINATOR_LEN..])
        .map_err(|e| DeserializeError::BorshError(e.to_string()))
}

/// Parse the token balance from raw SPL Token account data.
/// Amount is a u64 at byte offset 64 in the SPL Token account layout.
pub fn parse_token_amount(data: &[u8]) -> Result<u64, DeserializeError> {
    if data.len() < 72 {
        return Err(DeserializeError::DataTooShort);
    }
    Ok(u64::from_le_bytes(
        data[64..72].try_into().unwrap(),
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum DeserializeError {
    #[error("Account data too short")]
    DataTooShort,
    #[error("Borsh deserialization failed: {0}")]
    BorshError(String),
}

/// PDA derivation constants and helpers.
pub mod pda {
    use solana_pubkey::Pubkey;

    pub const MARKET_SEED: &[u8] = b"market";
    pub const VAULT_BASE_SEED: &[u8] = b"vault_base";
    pub const VAULT_QUOTE_SEED: &[u8] = b"vault_quote";
    pub const CONFIG_SEED: &[u8] = b"config";

    pub fn derive_market(program_id: &Pubkey, base_mint: &Pubkey, quote_mint: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[MARKET_SEED, base_mint.as_ref(), quote_mint.as_ref()],
            program_id,
        )
    }

    pub fn derive_vault_base(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[VAULT_BASE_SEED, market.as_ref()], program_id)
    }

    pub fn derive_vault_quote(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[VAULT_QUOTE_SEED, market.as_ref()], program_id)
    }

    pub fn derive_global_config(program_id: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[CONFIG_SEED], program_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_token_amount() {
        let mut data = vec![0u8; 165]; // SPL Token account size
        let amount: u64 = 1_000_000_000;
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        assert_eq!(parse_token_amount(&data).unwrap(), amount);
    }

    #[test]
    fn test_deserialize_error_too_short() {
        let data = vec![0u8; 4];
        assert!(deserialize_market(&data).is_err());
    }
}
