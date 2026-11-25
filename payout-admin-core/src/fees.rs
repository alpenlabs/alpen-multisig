//! Fee rates in integer tenths of a sat/vB (the 0.1 sat/vB step of the design), so fee math is
//! exact.

/// 0.1 sat/vB, Bitcoin Core's default minimum relay fee rate.
pub const MIN_FEE_RATE_TENTHS: u64 = 1;
/// 10,000 sat/vB.
pub const MAX_FEE_RATE_TENTHS: u64 = 100_000;

/// The requested rate falls outside the payout fee range.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeeRateError {
    #[error("The minimum fee rate is 0.1 sat/vB.")]
    TooLow,
    #[error("The maximum fee rate is 10,000 sat/vB.")]
    TooHigh,
}

/// Payout fee rate in tenths of a sat/vB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeRate(u64);

impl FeeRate {
    /// Accepts rates from 1 through 100,000 tenths of a sat/vB, inclusive.
    pub fn new(tenths: u64) -> Result<Self, FeeRateError> {
        if tenths < MIN_FEE_RATE_TENTHS {
            return Err(FeeRateError::TooLow);
        }

        if MAX_FEE_RATE_TENTHS < tenths {
            return Err(FeeRateError::TooHigh);
        }

        Ok(Self(tenths))
    }

    /// Smallest fee in sat that meets the rate for this vsize: ceil(rate × vsize).
    pub fn fee_for(self, vsize: u32) -> bitcoin::Amount {
        bitcoin::Amount::from_sat((self.0 * u64::from(vsize)).div_ceil(10))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_rates() {
        for rate in [0, 100_001, u64::MAX] {
            assert!(FeeRate::new(rate).is_err());
        }
    }

    #[test]
    fn fee_rounds_up() {
        assert_eq!(FeeRate::new(1).unwrap().fee_for(141).to_sat(), 15);
        assert_eq!(FeeRate::new(10).unwrap().fee_for(141).to_sat(), 141);
        assert_eq!(FeeRate::new(25).unwrap().fee_for(3).to_sat(), 8);
    }
}
