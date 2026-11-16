//! Fee rates in integer tenths of a sat/vB (the 0.1 sat/vB step of the design), so fee math is
//! exact.

/// 0.1 sat/vB, Bitcoin Core's default minimum relay fee rate.
pub const MIN_FEE_RATE_TENTHS: u64 = 1;
/// 10,000 sat/vB.
pub const MAX_FEE_RATE_TENTHS: u64 = 100_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeeRateError {
    #[error("Enter a fee rate in sat/vB with at most one decimal place, e.g. 2 or 12.5.")]
    Format,
    #[error("The minimum fee rate is 0.1 sat/vB.")]
    TooLow,
    #[error("The maximum fee rate is 10,000 sat/vB.")]
    TooHigh,
}

pub fn check_fee_rate(tenths: u64) -> Result<u64, FeeRateError> {
    if tenths < MIN_FEE_RATE_TENTHS {
        return Err(FeeRateError::TooLow);
    }
    if tenths > MAX_FEE_RATE_TENTHS {
        return Err(FeeRateError::TooHigh);
    }
    Ok(tenths)
}

/// Parses "2", "12.5" (at most one decimal) into tenths of a sat/vB.
pub fn parse_fee_rate(input: &str) -> Result<u64, FeeRateError> {
    let s = input.trim();
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, "0"),
    };
    if whole.is_empty()
        || frac.len() != 1
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || whole.len() > 9
    {
        return Err(FeeRateError::Format);
    }
    let tenths = whole.parse::<u64>().map_err(|_| FeeRateError::Format)? * 10
        + frac.parse::<u64>().map_err(|_| FeeRateError::Format)?;
    check_fee_rate(tenths)
}

/// Smallest fee in sat that meets the rate for this vsize: ceil(rate × vsize).
pub fn fee_for(tenths: u64, vsize: u64) -> u64 {
    (tenths * vsize).div_ceil(10)
}

pub fn format_fee_rate(tenths: u64) -> String {
    format!("{}.{} sat/vB", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tenths() {
        assert_eq!(parse_fee_rate("2"), Ok(20));
        assert_eq!(parse_fee_rate("12.5"), Ok(125));
        assert_eq!(parse_fee_rate("0.1"), Ok(1));
        assert_eq!(parse_fee_rate("10000"), Ok(100_000));
        assert_eq!(parse_fee_rate("0"), Err(FeeRateError::TooLow));
        assert_eq!(parse_fee_rate("10000.1"), Err(FeeRateError::TooHigh));
        assert_eq!(parse_fee_rate("1.25"), Err(FeeRateError::Format));
        assert_eq!(parse_fee_rate("-1"), Err(FeeRateError::Format));
        assert_eq!(parse_fee_rate("1."), Err(FeeRateError::Format));
        assert_eq!(parse_fee_rate(""), Err(FeeRateError::Format));
    }

    #[test]
    fn fee_rounds_up() {
        assert_eq!(fee_for(1, 141), 15);
        assert_eq!(fee_for(10, 141), 141);
        assert_eq!(fee_for(25, 3), 8);
        assert_eq!(format_fee_rate(125), "12.5 sat/vB");
    }
}
