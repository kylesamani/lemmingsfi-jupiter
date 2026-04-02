# LemmingsFi — Jupiter Integration Package

## Program

| Field | Value |
|-------|-------|
| **Program ID** | `BQEJZUB4CzoT6UhRffoCkqCyqQNrCPCSGHcPEmsdbEsX` |
| **Network** | Solana Mainnet |
| **Framework** | Anchor 0.31.1 |

## Live Markets

| Pair | Market Address | Base Mint | Quote Mint |
|------|---------------|-----------|------------|
| USDC/USDT | `5pVYETSDvAsr649PVppJDs885pSqWEN4pKZ83HuG7kpc` | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` (USDC, 6 decimals) | `Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB` (USDT, 6 decimals) |

## What's Included

```
├── README.md                  # This file
├── sdk/                       # Rust SDK implementing jupiter-amm-interface Amm trait
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # LemmingsFiAmm — full Amm trait implementation
│       ├── quote.rs           # Quoting engine (exact match to on-chain math)
│       └── state.rs           # Market/GlobalConfig deserialization + PDA helpers
├── idl/
│   └── lemmingsfi.json           # Anchor IDL (full type-safe instruction/account defs)
└── docs/
    ├── market-layout.md       # Byte-level account layout with offsets
    └── dflow-integration.md   # Technical integration guide (swap math, accounts, errors)
```

## SDK Overview

The `sdk/` crate implements `jupiter_amm_interface::Amm` for LemmingsFi markets. It:

- Deserializes on-chain Market state via `from_keyed_account()`
- Fetches market + vault + global_config accounts via `get_accounts_to_update()`
- Computes swap quotes via `quote()` — identical math to on-chain `compute_swap_output`
- Builds swap instructions via `get_swap_and_account_metas()`
- Reports inactive pools via `is_active()` (checks market pause + global pause)
- Caps quotes at vault liquidity to prevent failed transactions

### Dependencies

```toml
jupiter-amm-interface = { git = "https://github.com/jup-ag/jupiter-amm-interface", branch = "main" }
```

### Note on `Swap` Variant

The SDK currently uses `Swap::TokenSwap` as a placeholder. Jupiter will add a `LemmingsFi` variant to the `Swap` enum in `jupiter-amm-interface` after integration review.

### Tests

```bash
cd sdk && cargo test
# 18 tests pass — quoting parity, trait methods, vault caps, pause flags
```

## Architecture

LemmingsFi is a **proprietary AMM**:

- **Oracle-driven pricing**: An off-chain orchestrator pushes price updates via `update_oracle` (authority-signed)
- **Configurable spreads**: Bid/ask spreads set dynamically based on volatility + inventory
- **Private vaults**: Liquidity is managed by the protocol operator, not passive LPs
- **MEV-resistant**: Prices are authority-signed and time-bounded (max staleness)

```
Oracle Updater (off-chain) ──update_oracle──> Market Account (on-chain)
                                                      │
Jupiter Router ──────reads state──────────────> Market + Vaults
                                                      │
Jupiter Router ──────calls swap───────────────> Swap Instruction
```

## Swap Instruction

**Discriminator**: `sha256("global:swap")[..8]` = `[0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8]`

**Instruction Data (25 bytes)**:

| Offset | Size | Type | Field |
|--------|------|------|-------|
| 0 | 8 | `[u8; 8]` | discriminator |
| 8 | 1 | u8 | direction (0 = BuyBase, 1 = SellBase) |
| 9 | 8 | u64 | amount_in |
| 17 | 8 | u64 | min_amount_out |

**Accounts (8)**:

| # | Account | Writable | Signer | Description |
|---|---------|----------|--------|-------------|
| 0 | user | No | Yes | Transaction signer |
| 1 | global_config | No | No | GlobalConfig PDA (`seeds = ["config"]`) |
| 2 | market | Yes | No | Market PDA |
| 3 | vault_base | Yes | No | Base token vault |
| 4 | vault_quote | Yes | No | Quote token vault |
| 5 | user_base | Yes | No | User's base token account |
| 6 | user_quote | Yes | No | User's quote token account |
| 7 | token_program | No | No | SPL Token (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`) |

## Pricing Formula

**BuyBase** (user pays quote, receives base):
```
effective_ask = oracle_price × (10000 + ask_spread_bps) × (10000 + fee_bps) / (10000²)
base_out = quote_in × PRICE_SCALE / effective_ask
```

**SellBase** (user pays base, receives quote):
```
effective_bid = oracle_price × (10000 - bid_spread_bps) × (10000 - fee_bps) / (10000²)
quote_out = base_in × effective_bid / PRICE_SCALE
```

Where `PRICE_SCALE = 1,000,000`. All intermediate math uses `u128`. Integer division truncates.

**Oracle Age Spread Penalty**: On-chain, spreads widen linearly as the oracle ages:
```
age_penalty_bps = min(slots_since_oracle_update / 2, max_staleness_slots / 2)
effective_bid = bid_spread_bps + age_penalty_bps  (capped at 10,000)
effective_ask = ask_spread_bps + age_penalty_bps  (capped at 10,000)
```
The SDK provides `oracle_age_spread_penalty()` and `QuoteInput::from_market_with_age()` for accurate off-chain quoting.

## Error Codes

| Code | Name | Description |
|------|------|-------------|
| 6001 | MarketPaused | Market-level pause |
| 6002 | GlobalPaused | Global kill switch |
| 6003 | StaleOracle | Oracle too old (`current_slot - oracle_slot > max_staleness_slots`) |
| 6006 | SlippageExceeded | Output below `min_amount_out` |
| 6007 | OrderTooSmall | Below `min_order_size` |
| 6008 | OrderTooLarge | Above `max_order_size` |
| 6009 | InsufficientLiquidity | Vault can't cover output |

## Contact

Kyle Samani — iam@kylesamani.com
