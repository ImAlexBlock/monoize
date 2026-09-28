pub use crate::store_billing::money::{Currency, ExchangeRateRational};
use num_bigint::BigInt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("amount must be a canonical signed integer string")]
    InvalidAmount,
    #[error("exchange rate must be positive")]
    InvalidExchangeRate,
    #[error("amount exceeds the supported integer range")]
    AmountOverflow,
    #[error("accounting epoch {0} is unsupported")]
    UnsupportedEpoch(u64),
    #[error("currency {currency:?} contradicts accounting epoch {epoch}")]
    CurrencyEpochMismatch { currency: Currency, epoch: u64 },
    #[error("amounts must have the same currency and accounting epoch")]
    IncompatibleAmounts,
}

/// Signed nano-units with an explicit accounting denomination.
/// Epoch 0 is USD; epoch 1 is CNY. JSON stores `amount` as canonical decimal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NanoMoney {
    pub amount: i128,
    pub currency: Currency,
    pub epoch: u64,
}

impl NanoMoney {
    /// Constructs an amount only when its currency agrees with a supported epoch.
    pub fn new(amount: i128, currency: Currency, epoch: u64) -> Result<Self, MoneyError> {
        let money = Self {
            amount,
            currency,
            epoch,
        };
        money.validate()?;
        Ok(money)
    }

    /// Validates the denomination, including values constructed through public fields.
    pub fn validate(&self) -> Result<(), MoneyError> {
        match (self.currency, self.epoch) {
            (Currency::USD, 0) | (Currency::CNY, 1) => Ok(()),
            (_, 0 | 1) => Err(MoneyError::CurrencyEpochMismatch {
                currency: self.currency,
                epoch: self.epoch,
            }),
            (_, epoch) => Err(MoneyError::UnsupportedEpoch(epoch)),
        }
    }

    /// Adds matching denominations; mixed epochs and i128 overflow are errors.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.require_compatible(other)?;
        let amount = self
            .amount
            .checked_add(other.amount)
            .ok_or(MoneyError::AmountOverflow)?;
        Ok(Self { amount, ..self })
    }

    /// Subtracts matching denominations; a negative result remains valid.
    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.require_compatible(other)?;
        let amount = self
            .amount
            .checked_sub(other.amount)
            .ok_or(MoneyError::AmountOverflow)?;
        Ok(Self { amount, ..self })
    }

    /// Converts with the caller's explicit rate and validates both source and target epochs.
    pub fn convert_to(
        self,
        currency: Currency,
        epoch: u64,
        rate: &ExchangeRateRational,
    ) -> Result<Self, MoneyError> {
        self.validate()?;
        let target = Self::new(0, currency, epoch)?;
        let amount = convert_nano(self.amount, self.currency, currency, rate)?;
        Ok(Self { amount, ..target })
    }

    fn require_compatible(self, other: Self) -> Result<(), MoneyError> {
        self.validate()?;
        other.validate()?;
        if self.currency != other.currency || self.epoch != other.epoch {
            return Err(MoneyError::IncompatibleAmounts);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct WireNanoMoney {
    amount: String,
    currency: Currency,
    epoch: u64,
}

impl Serialize for NanoMoney {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        WireNanoMoney {
            amount: self.amount.to_string(),
            currency: self.currency,
            epoch: self.epoch,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NanoMoney {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireNanoMoney::deserialize(deserializer)?;
        let amount = parse_amount(&wire.amount).map_err(serde::de::Error::custom)?;
        Self::new(amount, wire.currency, wire.epoch).map_err(serde::de::Error::custom)
    }
}

fn parse_amount(value: &str) -> Result<i128, MoneyError> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty()
        || value == "-0"
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(MoneyError::InvalidAmount);
    }
    value.parse().map_err(|_| MoneyError::AmountOverflow)
}

/// Converts signed nano-units using CNY per USD and rounds half away from zero once.
/// Only an unrepresentable final result overflows; same-currency amounts are unchanged.
pub fn convert_nano(
    amount: i128,
    source: Currency,
    target: Currency,
    rate: &ExchangeRateRational,
) -> Result<i128, MoneyError> {
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return Err(MoneyError::InvalidExchangeRate);
    }
    if source == target {
        return Ok(amount);
    }
    let (multiplier, divisor) = match (source, target) {
        (Currency::USD, Currency::CNY) => (rate.numerator(), rate.denominator()),
        (Currency::CNY, Currency::USD) => (rate.denominator(), rate.numerator()),
        _ => unreachable!(),
    };
    // The magnitude of i128::MIN and representable quotients can both require
    // products beyond i128; round the widened magnitude before restoring its sign.
    let numerator = BigInt::from(amount.unsigned_abs()) * multiplier;
    let denominator = BigInt::from(divisor);
    let mut rounded = &numerator / &denominator;
    let remainder = numerator % &denominator;
    if remainder * 2_u8 >= denominator {
        rounded += 1_u8;
    }
    if amount < 0 {
        rounded = -rounded;
    }
    i128::try_from(rounded).map_err(|_| MoneyError::AmountOverflow)
}
