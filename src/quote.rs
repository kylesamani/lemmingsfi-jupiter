/// Shared swap quoting engine for LemmingsFi.
/// Produces identical results to on-chain `compute_swap_output` in
/// `programs/lemmingsfi/src/instructions/swap.rs:183-235`.

pub const PRICE_SCALE: u64 = 1_000_000;
pub const BPS_DENOMINATOR: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    /// User pays quote tokens, receives base tokens.
    BuyBase,
    /// User pays base tokens, receives quote tokens.
    SellBase,
}

/// Input parameters for a swap quote.
#[derive(Debug, Clone)]
pub struct QuoteInput {
    pub oracle_price: u64,
    pub bid_spread_bps: u16,
    pub ask_spread_bps: u16,
    pub fee_bps: u16,
}

/// Result of a swap quote.
#[derive(Debug, Clone)]
pub struct QuoteResult {
    /// Amount of output tokens.
    pub amount_out: u64,
    /// The effective price used (in PRICE_SCALE units).
    pub effective_price: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum QuoteError {
    #[error("Math overflow in swap computation")]
    MathOverflow,
    #[error("Zero output amount")]
    ZeroOutput,
}

/// Compute the output amount for a swap.
/// This must produce identical results to the on-chain `compute_swap_output` function.
pub fn compute_swap_output(
    input: &QuoteInput,
    direction: SwapDirection,
    amount_in: u64,
) -> Result<QuoteResult, QuoteError> {
    let price = input.oracle_price as u128;
    let bps = BPS_DENOMINATOR as u128;
    let scale = PRICE_SCALE as u128;
    let amount = amount_in as u128;

    match direction {
        SwapDirection::BuyBase => {
            let ask_spread = input.ask_spread_bps as u128;
            let fee = input.fee_bps as u128;

            // effective_ask = price * (bps + ask_spread) * (bps + fee) / (bps * bps)
            let effective_ask = price
                .checked_mul(bps.checked_add(ask_spread).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?
                .checked_mul(bps.checked_add(fee).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?
                .checked_div(bps.checked_mul(bps).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?;

            // base_out = quote_in * scale / effective_ask
            let base_out = amount
                .checked_mul(scale)
                .ok_or(QuoteError::MathOverflow)?
                .checked_div(effective_ask)
                .ok_or(QuoteError::MathOverflow)?;

            Ok(QuoteResult {
                amount_out: base_out as u64,
                effective_price: effective_ask as u64,
            })
        }
        SwapDirection::SellBase => {
            let bid_spread = input.bid_spread_bps as u128;
            let fee = input.fee_bps as u128;

            // effective_bid = price * (bps - bid_spread) * (bps - fee) / (bps * bps)
            let effective_bid = price
                .checked_mul(bps.checked_sub(bid_spread).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?
                .checked_mul(bps.checked_sub(fee).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?
                .checked_div(bps.checked_mul(bps).ok_or(QuoteError::MathOverflow)?)
                .ok_or(QuoteError::MathOverflow)?;

            // quote_out = base_in * effective_bid / scale
            let quote_out = amount
                .checked_mul(effective_bid)
                .ok_or(QuoteError::MathOverflow)?
                .checked_div(scale)
                .ok_or(QuoteError::MathOverflow)?;

            Ok(QuoteResult {
                amount_out: quote_out as u64,
                effective_price: effective_bid as u64,
            })
        }
    }
}

/// Compute an additive spread penalty (in bps) based on oracle age.
/// Linearly scales from 0 at slot 0 to max_staleness_slots/2 bps at max_staleness_slots.
/// E.g., with max_staleness_slots=100, a 50-slot-old oracle adds 25 bps to each side.
/// This makes stale-oracle arbitrage progressively more expensive.
/// Matches on-chain `swap_common::oracle_age_spread_penalty` exactly.
pub fn oracle_age_spread_penalty(oracle_slot: u64, current_slot: u64, max_staleness_slots: u64) -> u16 {
    let slots_since = current_slot.saturating_sub(oracle_slot);
    let max_penalty = max_staleness_slots / 2;
    let penalty = (slots_since / 2).min(max_penalty);
    penalty as u16
}

/// Convenience: create QuoteInput from a MarketState (no age penalty).
impl From<&crate::state::MarketState> for QuoteInput {
    fn from(market: &crate::state::MarketState) -> Self {
        Self {
            oracle_price: market.oracle_price,
            bid_spread_bps: market.bid_spread_bps,
            ask_spread_bps: market.ask_spread_bps,
            fee_bps: market.fee_bps,
        }
    }
}

impl QuoteInput {
    /// Create QuoteInput from a MarketState with oracle age spread penalty applied.
    /// `current_slot` should be the latest slot from the cluster.
    /// This produces quotes that match on-chain behavior where spreads widen
    /// as the oracle ages.
    pub fn from_market_with_age(market: &crate::state::MarketState, current_slot: u64) -> Self {
        let penalty = oracle_age_spread_penalty(
            market.oracle_slot,
            current_slot,
            market.max_staleness_slots,
        );
        Self {
            oracle_price: market.oracle_price,
            bid_spread_bps: market.bid_spread_bps.saturating_add(penalty).min(10_000),
            ask_spread_bps: market.ask_spread_bps.saturating_add(penalty).min(10_000),
            fee_bps: market.fee_bps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buy_base_zero_spread_zero_fee() {
        let input = QuoteInput {
            oracle_price: 150_000_000,
            bid_spread_bps: 0,
            ask_spread_bps: 0,
            fee_bps: 0,
        };
        let result = compute_swap_output(&input, SwapDirection::BuyBase, 150_000_000).unwrap();
        assert_eq!(result.amount_out, 1_000_000);
        assert_eq!(result.effective_price, 150_000_000);
    }

    #[test]
    fn test_sell_base_zero_spread_zero_fee() {
        let input = QuoteInput {
            oracle_price: 150_000_000,
            bid_spread_bps: 0,
            ask_spread_bps: 0,
            fee_bps: 0,
        };
        let result = compute_swap_output(&input, SwapDirection::SellBase, 1_000_000).unwrap();
        assert_eq!(result.amount_out, 150_000_000);
        assert_eq!(result.effective_price, 150_000_000);
    }

    #[test]
    fn test_buy_base_with_spread_and_fee() {
        let input = QuoteInput {
            oracle_price: 150_000_000,
            bid_spread_bps: 100,
            ask_spread_bps: 100, // 1%
            fee_bps: 30,         // 0.3%
        };
        let result = compute_swap_output(&input, SwapDirection::BuyBase, 150_000_000).unwrap();
        // Should get less than 1_000_000 base due to spread + fee
        assert!(result.amount_out < 1_000_000);
        assert!(result.amount_out > 0);
        assert!(result.effective_price > 150_000_000);
    }

    #[test]
    fn test_sell_base_with_spread_and_fee() {
        let input = QuoteInput {
            oracle_price: 150_000_000,
            bid_spread_bps: 100,
            ask_spread_bps: 100,
            fee_bps: 30,
        };
        let result = compute_swap_output(&input, SwapDirection::SellBase, 1_000_000).unwrap();
        // Should get less than 150_000_000 quote due to spread + fee
        assert!(result.amount_out < 150_000_000);
        assert!(result.amount_out > 0);
        assert!(result.effective_price < 150_000_000);
    }

    #[test]
    fn test_max_spread_max_fee() {
        let input = QuoteInput {
            oracle_price: 150_000_000,
            bid_spread_bps: 10_000,
            ask_spread_bps: 10_000,
            fee_bps: 10_000,
        };
        // BuyBase: effective_ask = price * 2 * 2 = 4x price
        let buy = compute_swap_output(&input, SwapDirection::BuyBase, 150_000_000).unwrap();
        assert_eq!(buy.amount_out, 250_000);

        // SellBase: effective_bid = price * 0 * 0 = 0, output = 0
        let sell = compute_swap_output(&input, SwapDirection::SellBase, 1_000_000).unwrap();
        assert_eq!(sell.amount_out, 0);
    }

    /// Verify SDK math exactly matches the test harness reference math.
    #[test]
    fn test_parity_with_reference() {
        let prices = [1, 100_000, 150_000_000, 1_000_000_000];
        let spreads = [0, 50, 100, 500, 5000];
        let fees = [0, 10, 30, 100, 1000];
        let amounts = [1, 1_000, 1_000_000, 1_000_000_000];

        for &price in &prices {
            for &spread in &spreads {
                for &fee in &fees {
                    for &amount in &amounts {
                        let input = QuoteInput {
                            oracle_price: price,
                            bid_spread_bps: spread,
                            ask_spread_bps: spread,
                            fee_bps: fee,
                        };

                        // BuyBase
                        let sdk_buy = compute_swap_output(&input, SwapDirection::BuyBase, amount);
                        let ref_buy = reference_buy(price, spread, fee, amount);
                        match (sdk_buy, ref_buy) {
                            (Ok(r), Some(v)) => assert_eq!(r.amount_out, v,
                                "BuyBase mismatch: price={price} spread={spread} fee={fee} amount={amount}"),
                            (Err(_), None) => {} // both overflow/error
                            _ => panic!("BuyBase result mismatch: price={price} spread={spread} fee={fee} amount={amount}"),
                        }

                        // SellBase
                        let sdk_sell = compute_swap_output(&input, SwapDirection::SellBase, amount);
                        let ref_sell = reference_sell(price, spread, fee, amount);
                        match (sdk_sell, ref_sell) {
                            (Ok(r), Some(v)) => assert_eq!(r.amount_out, v,
                                "SellBase mismatch: price={price} spread={spread} fee={fee} amount={amount}"),
                            (Err(_), None) => {}
                            _ => panic!("SellBase result mismatch: price={price} spread={spread} fee={fee} amount={amount}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_oracle_age_spread_penalty_zero() {
        // Same slot = 0 penalty
        assert_eq!(oracle_age_spread_penalty(100, 100, 200), 0);
    }

    #[test]
    fn test_oracle_age_spread_penalty_linear() {
        // 50 slots old, max_staleness=200 → penalty = 50/2 = 25 bps
        assert_eq!(oracle_age_spread_penalty(100, 150, 200), 25);
    }

    #[test]
    fn test_oracle_age_spread_penalty_capped() {
        // 1000 slots old, max_staleness=200 → capped at 200/2 = 100 bps
        assert_eq!(oracle_age_spread_penalty(100, 1100, 200), 100);
    }

    #[test]
    fn test_oracle_age_spread_penalty_at_max() {
        // Exactly at max staleness: penalty = 200/2 = 100
        assert_eq!(oracle_age_spread_penalty(100, 300, 200), 100);
    }

    /// Reference implementation matching tests/src/helpers/math.rs
    fn reference_buy(oracle_price: u64, ask_spread_bps: u16, fee_bps: u16, quote_in: u64) -> Option<u64> {
        let price = oracle_price as u128;
        let bps = BPS_DENOMINATOR as u128;
        let scale = PRICE_SCALE as u128;
        let amount = quote_in as u128;
        let ask = ask_spread_bps as u128;
        let fee = fee_bps as u128;

        let effective_ask = price
            .checked_mul(bps.checked_add(ask)?)?
            .checked_mul(bps.checked_add(fee)?)?
            .checked_div(bps.checked_mul(bps)?)?;

        let base_out = amount.checked_mul(scale)?.checked_div(effective_ask)?;
        Some(base_out as u64)
    }

    fn reference_sell(oracle_price: u64, bid_spread_bps: u16, fee_bps: u16, base_in: u64) -> Option<u64> {
        let price = oracle_price as u128;
        let bps = BPS_DENOMINATOR as u128;
        let scale = PRICE_SCALE as u128;
        let amount = base_in as u128;
        let bid = bid_spread_bps as u128;
        let fee = fee_bps as u128;

        let effective_bid = price
            .checked_mul(bps.checked_sub(bid)?)?
            .checked_mul(bps.checked_sub(fee)?)?
            .checked_div(bps.checked_mul(bps)?)?;

        let quote_out = amount.checked_mul(effective_bid)?.checked_div(scale)?;
        Some(quote_out as u64)
    }
}
