//! Dynamic taker fee calculator.
//! Makers pay ZERO fees — this is for awareness of taker economics only.

/// Maker fee rate is always zero.
pub const MAKER_FEE_RATE: f64 = 0.0;

/// Dynamic taker fee calculator.
pub struct FeeCalculator;

impl FeeCalculator {
    pub fn new() -> Self {
        Self
    }

    /// Taker fee rate as a fraction at probability p.
    /// Formula: 0.25 * (p * (1-p))^2
    /// Max at p=0.50: 0.015625 (1.5625%)
    pub fn taker_fee_rate(p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        0.25 * (p * (1.0 - p)).powi(2)
    }

    /// Taker fee in USDC for `shares` at probability `p`.
    pub fn taker_fee(shares: f64, p: f64) -> f64 {
        shares * Self::taker_fee_rate(p)
    }

    /// Minimum edge (in bps) a taker needs to break even at probability p.
    pub fn taker_breakeven_edge_bps(p: f64) -> f64 {
        Self::taker_fee_rate(p) * 10_000.0
    }
}
