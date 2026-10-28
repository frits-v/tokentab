use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign};

use serde::{Serialize, Serializer};

pub const NANOS_PER_DOLLAR: u64 = 1_000_000_000;

/// Whole nanodollars. Every published per-million-token price is a whole
/// number of nanodollars per token, so sums over billions of tokens stay exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Money(u64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub fn from_nanos(nanos: u64) -> Money {
        Money(nanos)
    }

    /// Rounds to the nearest nanodollar. Panics on a negative or non-finite amount:
    /// prices and fees are validated at the loader boundary, so this is a programming error.
    pub fn from_usd(usd: f64) -> Money {
        assert!(usd.is_finite() && usd >= 0.0, "money must be finite and non-negative, got {usd}");
        Money((usd * NANOS_PER_DOLLAR as f64).round() as u64)
    }

    pub fn nanos(self) -> u64 {
        self.0
    }

    pub fn usd(self) -> f64 {
        self.0 as f64 / NANOS_PER_DOLLAR as f64
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn saturating_sub(self, other: Money) -> Money {
        Money(self.0.saturating_sub(other.0))
    }

    /// Scales by a non-negative factor, rounding to the nearest nanodollar.
    pub fn scale(self, factor: f64) -> Money {
        assert!(factor.is_finite() && factor >= 0.0, "scale factor must be finite and non-negative, got {factor}");
        Money((self.0 as f64 * factor).round() as u64)
    }

    pub fn times(self, count: u64) -> Money {
        Money(self.0.checked_mul(count).expect("money multiplication overflowed u64 nanodollars"))
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, other: Money) -> Money {
        Money(self.0.checked_add(other.0).expect("money addition overflowed u64 nanodollars"))
    }
}

impl AddAssign for Money {
    fn add_assign(&mut self, other: Money) {
        *self = *self + other;
    }
}

impl Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, Add::add)
    }
}

/// Formats as dollars and cents with thousands separators: `$1,234.57`.
impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cents = (self.0 + NANOS_PER_DOLLAR / 200) / (NANOS_PER_DOLLAR / 100);
        let dollars = cents / 100;
        let remainder = cents % 100;
        let digits = dollars.to_string();
        let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        write!(f, "${grouped}.{remainder:02}")
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.usd())
    }
}

/// A price in nanodollars per token.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PerToken(u64);

impl PerToken {
    /// Converts a published "$ per million tokens" figure. Panics when the figure
    /// is not a whole number of nanodollars per token; the loader validates first.
    pub fn from_usd_per_mtok(usd_per_mtok: f64) -> PerToken {
        assert!(usd_per_mtok.is_finite() && usd_per_mtok >= 0.0, "price must be finite and non-negative, got {usd_per_mtok}");
        let nanos_per_token = usd_per_mtok * 1_000.0;
        let rounded = nanos_per_token.round();
        assert!((nanos_per_token - rounded).abs() < 1e-6, "price {usd_per_mtok}/MTok is not a whole number of nanodollars per token");
        PerToken(rounded as u64)
    }

    pub fn nanos_per_token(self) -> u64 {
        self.0
    }

    pub fn usd_per_mtok(self) -> f64 {
        self.0 as f64 / 1_000.0
    }

    pub fn cost(self, tokens: u64) -> Money {
        Money(self.0.checked_mul(tokens).expect("token cost overflowed u64 nanodollars"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_prices_are_whole_nanodollars_per_token() {
        for price in [10.0, 12.5, 20.0, 0.25, 50.0, 0.125, 0.625, 0.01, 0.05, 1.0, 0.10, 0.20, 2.50] {
            let per_token = PerToken::from_usd_per_mtok(price);
            assert!((per_token.usd_per_mtok() - price).abs() < 1e-9);
        }
    }

    #[test]
    fn cost_of_one_million_tokens_is_the_listed_price() {
        let cost = PerToken::from_usd_per_mtok(12.5).cost(1_000_000);
        assert_eq!(cost, Money::from_usd(12.5));
    }

    #[test]
    fn display_groups_thousands_and_rounds_half_up() {
        assert_eq!(Money::from_usd(1234.565).to_string(), "$1,234.57");
        assert_eq!(Money::from_usd(0.004).to_string(), "$0.00");
        assert_eq!(Money::from_usd(1_000_000.0).to_string(), "$1,000,000.00");
        assert_eq!(Money::ZERO.to_string(), "$0.00");
    }

    #[test]
    fn scale_and_times_round_to_nanodollars() {
        assert_eq!(Money::from_usd(60.0).scale(0.5), Money::from_usd(30.0));
        assert_eq!(Money::from_usd(20.0).times(3), Money::from_usd(60.0));
    }
}
