//! Exact fixed-point ratios.
//!
//! Every rate in this crate is a ratio of two integer counters, so binary
//! floating point is never involved: a rate is stored as **basis points**
//! (parts per 10 000) computed with integer division. That keeps comparisons
//! against thresholds exact and reproducible, which matters because these
//! numbers drive operational decisions like disabling a collector.

use serde::{Deserialize, Serialize};

/// Basis points in a whole, i.e. `10_000 bp == 1.0 == 100%`.
pub const BASIS_POINTS_PER_WHOLE: i64 = 10_000;

/// A ratio held as basis points.
///
/// Two renderings are provided because this crate has two kinds of ratio:
/// bounded rates in `[0, 1]` (use [`Ratio::percent_string`]) and open-ended
/// "x per y" ratios such as requests per discovery (use
/// [`Ratio::decimal_string`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ratio {
    /// Parts per 10 000.
    pub basis_points: i64,
}

impl Ratio {
    /// Exactly zero.
    pub const ZERO: Ratio = Ratio { basis_points: 0 };
    /// Exactly one (100%).
    pub const ONE: Ratio = Ratio {
        basis_points: BASIS_POINTS_PER_WHOLE,
    };

    /// Build from a numerator and denominator.
    ///
    /// Returns `None` when the denominator is zero — an undefined rate is
    /// reported as "no data" rather than silently becoming `0`, because
    /// "0% parse failures" and "never ran" must not look alike.
    ///
    /// The intermediate product is computed in `i128`, so a large counter
    /// cannot overflow into a wrong answer; a result too large for `i64`
    /// returns `None` rather than wrapping.
    pub fn from_ratio(numerator: i64, denominator: i64) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let scaled = (numerator as i128) * (BASIS_POINTS_PER_WHOLE as i128) / (denominator as i128);
        i64::try_from(scaled)
            .ok()
            .map(|basis_points| Self { basis_points })
    }

    /// Construct directly from basis points.
    pub const fn from_basis_points(basis_points: i64) -> Self {
        Self { basis_points }
    }

    /// Whole percent, truncated. Handy for coarse threshold checks.
    pub const fn percent(self) -> i64 {
        self.basis_points / 100
    }

    /// Render as a percentage with two decimals, e.g. `12.34%`.
    pub fn percent_string(self) -> String {
        let hundredths = self.basis_points;
        let sign = if hundredths < 0 { "-" } else { "" };
        let abs = hundredths.abs();
        format!("{sign}{}.{:02}%", abs / 100, abs % 100)
    }

    /// Render as a plain decimal with two places, e.g. `3.50`.
    pub fn decimal_string(self) -> String {
        let sign = if self.basis_points < 0 { "-" } else { "" };
        let abs = self.basis_points.abs();
        format!(
            "{sign}{}.{:02}",
            abs / BASIS_POINTS_PER_WHOLE,
            (abs % BASIS_POINTS_PER_WHOLE) / 100
        )
    }

    /// Scale a penalty budget by this ratio, e.g. 40% of a 20-point budget.
    ///
    /// Saturating and integer-only; used by the quality score so weights stay
    /// exactly reproducible.
    pub fn scale(self, budget: i32) -> i32 {
        let scaled =
            (self.basis_points as i128) * (budget as i128) / (BASIS_POINTS_PER_WHOLE as i128);
        i32::try_from(scaled).unwrap_or(if scaled.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undefined_rates_are_none_not_zero() {
        assert_eq!(Ratio::from_ratio(0, 0), None);
        assert_eq!(Ratio::from_ratio(5, 0), None);
        assert_eq!(Ratio::from_ratio(0, 10), Some(Ratio::ZERO));
    }

    #[test]
    fn ratios_are_exact_integer_math() {
        assert_eq!(
            Ratio::from_ratio(1, 2),
            Some(Ratio::from_basis_points(5_000))
        );
        assert_eq!(Ratio::from_ratio(1, 1), Some(Ratio::ONE));
        assert_eq!(
            Ratio::from_ratio(7, 2),
            Some(Ratio::from_basis_points(35_000))
        );
        // 1/3 truncates deterministically rather than carrying float error.
        assert_eq!(
            Ratio::from_ratio(1, 3),
            Some(Ratio::from_basis_points(3_333))
        );
    }

    #[test]
    fn large_counters_do_not_overflow() {
        let big = i64::MAX / 2;
        assert_eq!(Ratio::from_ratio(big, big), Some(Ratio::ONE));
        // A result beyond i64 is refused rather than wrapped.
        assert_eq!(Ratio::from_ratio(i64::MAX, 1), None);
    }

    #[test]
    fn renderings_match_their_intended_use() {
        assert_eq!(Ratio::from_basis_points(1_234).percent_string(), "12.34%");
        assert_eq!(Ratio::ZERO.percent_string(), "0.00%");
        assert_eq!(Ratio::ONE.percent_string(), "100.00%");
        assert_eq!(Ratio::from_basis_points(-250).percent_string(), "-2.50%");

        assert_eq!(Ratio::from_basis_points(35_000).decimal_string(), "3.50");
        assert_eq!(Ratio::ONE.decimal_string(), "1.00");
        assert_eq!(Ratio::from_basis_points(5_000).decimal_string(), "0.50");
    }

    #[test]
    fn whole_percent_truncates() {
        assert_eq!(Ratio::from_basis_points(1_999).percent(), 19);
        assert_eq!(Ratio::ONE.percent(), 100);
    }

    #[test]
    fn scaling_a_penalty_budget_is_proportional() {
        assert_eq!(Ratio::ONE.scale(40), 40);
        assert_eq!(Ratio::from_basis_points(5_000).scale(40), 20);
        assert_eq!(Ratio::ZERO.scale(40), 0);
        assert_eq!(Ratio::from_basis_points(2_500).scale(20), 5);
    }
}
