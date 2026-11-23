#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Trading-domain primitives and a pure pre-trade risk gate for AJENTIR.
//!
//! This first slice supports whole-unit, limit-priced instruments in one
//! portfolio currency. It has no market feed, broker adapter, alpha model or
//! order-submission capability. Approval is not execution authorisation.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

/// Errors raised while constructing financial-domain values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// An identifier is empty, too long or contains unsupported characters.
    #[error("invalid {kind} identifier: {value}")]
    InvalidIdentifier { kind: &'static str, value: String },
    /// A currency code is not exactly three uppercase ASCII letters.
    #[error("currency code must be exactly three uppercase ASCII letters")]
    InvalidCurrency,
    /// A portfolio's reporting currency differs from its exposure currency.
    #[error("portfolio reporting and exposure currencies differ")]
    CurrencyMismatch,
    /// A price or quantity must be greater than zero.
    #[error("{0} must be greater than zero")]
    MustBePositive(&'static str),
}

/// Validated three-letter currency code.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency(String);

impl Currency {
    /// Creates a code such as USD, KES or EUR.
    pub fn new(code: impl Into<String>) -> Result<Self, DomainError> {
        let code = code.into();
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(DomainError::InvalidCurrency);
        }
        Ok(Self(code))
    }

    /// Returns the currency code.
    #[must_use]
    pub fn code(&self) -> &str { &self.0 }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.code()) }
}

macro_rules! identifier {
    ($name:ident, $kind:literal, $max:expr) => {
        #[doc = concat!("Validated ", $kind, " identifier.")]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);
        impl $name {
            #[doc = concat!("Creates a validated ", $kind, " identifier.")]
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                let valid = !value.is_empty() && value.len() <= $max
                    && value.bytes().all(|b| b.is_ascii_alphanumeric()
                        || matches!(b, b'-' | b'_' | b'.' | b':'));
                if !valid {
                    return Err(DomainError::InvalidIdentifier { kind: $kind, value });
                }
                Ok(Self(value))
            }
            /// Returns the identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str { &self.0 }
        }
    };
}

identifier!(InstrumentId, "instrument", 64);
identifier!(OrderId, "order", 128);
identifier!(StrategyId, "strategy", 128);

/// Non-negative amount expressed in minor currency units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Money {
    currency: Currency,
    minor_units: u64,
}

impl Money {
    /// Creates an amount in minor units (for example, cents).
    #[must_use]
    pub fn new(currency: Currency, minor_units: u64) -> Self {
        Self { currency, minor_units }
    }
    /// Returns the currency.
    #[must_use]
    pub fn currency(&self) -> &Currency { &self.currency }
    /// Returns the amount in minor units.
    #[must_use]
    pub fn minor_units(&self) -> u64 { self.minor_units }
}

/// Price of one whole instrument unit in minor currency units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Price {
    currency: Currency,
    minor_units_per_unit: u64,
}

impl Price {
    /// Creates a strictly positive unit price.
    pub fn new(currency: Currency, minor_units_per_unit: u64) -> Result<Self, DomainError> {
        if minor_units_per_unit == 0 { return Err(DomainError::MustBePositive("price")); }
        Ok(Self { currency, minor_units_per_unit })
    }
    /// Returns the price currency.
    #[must_use]
    pub fn currency(&self) -> &Currency { &self.currency }
    /// Returns minor units per whole instrument unit.
    #[must_use]
    pub fn minor_units_per_unit(&self) -> u64 { self.minor_units_per_unit }
}

/// Positive whole-unit order quantity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quantity(u64);

impl Quantity {
    /// Creates a positive whole-unit quantity.
    pub fn new(units: u64) -> Result<Self, DomainError> {
        if units == 0 { return Err(DomainError::MustBePositive("quantity")); }
        Ok(Self(units))
    }
    /// Returns the quantity in whole units.
    #[must_use]
    pub fn units(self) -> u64 { self.0 }
}

/// Direction of a proposed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderSide {
    /// Increase a long position.
    Buy,
    /// Reduce an existing long position; short selling is not supported.
    Sell,
}

/// Validated limit-order proposal. This is not a broker order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderIntent {
    id: OrderId,
    strategy: StrategyId,
    instrument: InstrumentId,
    side: OrderSide,
    quantity: Quantity,
    limit_price: Price,
}

impl OrderIntent {
    /// Creates a limit-order proposal.
    #[must_use]
    pub fn new(id: OrderId, strategy: StrategyId, instrument: InstrumentId,
        side: OrderSide, quantity: Quantity, limit_price: Price) -> Self {
        Self { id, strategy, instrument, side, quantity, limit_price }
    }
    /// Returns the order ID.
    #[must_use]
    pub fn id(&self) -> &OrderId { &self.id }
    /// Returns the strategy ID.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId { &self.strategy }
    /// Returns the instrument ID.
    #[must_use]
    pub fn instrument(&self) -> &InstrumentId { &self.instrument }
    /// Returns the order side.
    #[must_use]
    pub fn side(&self) -> OrderSide { self.side }
    /// Returns the whole-unit quantity.
    #[must_use]
    pub fn quantity(&self) -> Quantity { self.quantity }
    /// Returns the limit price.
    #[must_use]
    pub fn limit_price(&self) -> &Price { &self.limit_price }
    /// Computes exact notional with checked integer arithmetic.
    #[must_use]
    pub fn notional(&self) -> Option<Money> {
        self.limit_price.minor_units_per_unit.checked_mul(self.quantity.units())
            .map(|amount| Money::new(self.limit_price.currency.clone(), amount))
    }
}

/// Immutable portfolio facts supplied to pre-trade risk evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioSnapshot {
    currency: Currency,
    gross_exposure: Money,
    positions: BTreeMap<InstrumentId, u64>,
    open_order_count: u64,
}

impl PortfolioSnapshot {
    /// Creates a snapshot, rejecting a reporting/exposure currency mismatch.
    pub fn new(currency: Currency, gross_exposure: Money,
        positions: BTreeMap<InstrumentId, u64>, open_order_count: u64)
        -> Result<Self, DomainError> {
        if gross_exposure.currency != currency {
            return Err(DomainError::CurrencyMismatch);
        }
        Ok(Self { currency, gross_exposure, positions, open_order_count })
    }
    /// Returns the reporting currency.
    #[must_use]
    pub fn currency(&self) -> &Currency { &self.currency }
    /// Returns current gross exposure.
    #[must_use]
    pub fn gross_exposure(&self) -> &Money { &self.gross_exposure }
    /// Returns the recorded long position, or zero.
    #[must_use]
    pub fn position(&self, instrument: &InstrumentId) -> u64 {
        self.positions.get(instrument).copied().unwrap_or(0)
    }
    /// Returns the number of open orders.
    #[must_use]
    pub fn open_order_count(&self) -> u64 { self.open_order_count }
}

/// Risk-policy construction errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RiskPolicyError {
    /// The policy's monetary limits use different currencies.
    #[error("risk limits must use the same currency")]
    CurrencyMismatch,
}

/// Hard pre-trade limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskLimits {
    max_order_notional: Money,
    max_gross_exposure: Money,
    max_open_orders: u64,
}

impl RiskLimits {
    /// Creates hard limits denominated in one currency.
    pub fn new(max_order_notional: Money, max_gross_exposure: Money,
        max_open_orders: u64) -> Result<Self, RiskPolicyError> {
        if max_order_notional.currency() != max_gross_exposure.currency() {
            return Err(RiskPolicyError::CurrencyMismatch);
        }
        Ok(Self { max_order_notional, max_gross_exposure, max_open_orders })
    }
}

/// Deterministic reason an order was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskReason {
    /// Exact notional or projected exposure overflowed.
    ArithmeticOverflow,
    /// Order, portfolio and policy currencies do not match.
    CurrencyMismatch,
    /// Order notional exceeds its configured maximum.
    OrderNotionalLimit { requested: u64, maximum: u64 },
    /// Projected gross exposure exceeds its configured maximum.
    GrossExposureLimit { projected: u64, maximum: u64 },
    /// The portfolio already has the maximum number of open orders.
    OpenOrderLimit { current: u64, maximum: u64 },
    /// A sell order exceeds the recorded long position.
    InsufficientPosition { requested: u64, available: u64 },
    /// Sell notional exceeds the supplied aggregate gross exposure.
    InconsistentPortfolioExposure,
}

/// Result of pure pre-trade risk evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskDecision {
    /// Supplied facts satisfy all configured hard limits.
    Approved {
        /// Exact order notional.
        order_notional: Money,
        /// Projected gross exposure after the order.
        projected_gross_exposure: Money,
    },
    /// The proposal must not be forwarded to an execution adapter.
    Rejected(RiskReason),
}

/// Pure, deterministic pre-trade evaluation.
///
/// Approval means only that the supplied snapshot satisfies these limits. It is
/// not a broker permit, a buying-power guarantee or an investment recommendation.
#[must_use]
pub fn evaluate_order(order: &OrderIntent, portfolio: &PortfolioSnapshot,
    limits: &RiskLimits) -> RiskDecision {
    let Some(notional) = order.notional() else {
        return RiskDecision::Rejected(RiskReason::ArithmeticOverflow);
    };
    if notional.currency() != portfolio.currency()
        || notional.currency() != limits.max_order_notional.currency()
        || portfolio.gross_exposure().currency() != limits.max_gross_exposure.currency() {
        return RiskDecision::Rejected(RiskReason::CurrencyMismatch);
    }
    if notional.minor_units() > limits.max_order_notional.minor_units() {
        return RiskDecision::Rejected(RiskReason::OrderNotionalLimit {
            requested: notional.minor_units(),
            maximum: limits.max_order_notional.minor_units(),
        });
    }
    if portfolio.open_order_count() >= limits.max_open_orders {
        return RiskDecision::Rejected(RiskReason::OpenOrderLimit {
            current: portfolio.open_order_count(), maximum: limits.max_open_orders,
        });
    }
    let projected = match order.side() {
        OrderSide::Buy => match portfolio.gross_exposure().minor_units()
            .checked_add(notional.minor_units()) {
                Some(value) => value,
                None => return RiskDecision::Rejected(RiskReason::ArithmeticOverflow),
            },
        OrderSide::Sell => {
            let available = portfolio.position(order.instrument());
            if order.quantity().units() > available {
                return RiskDecision::Rejected(RiskReason::InsufficientPosition {
                    requested: order.quantity().units(), available,
                });
            }
            if notional.minor_units() > portfolio.gross_exposure().minor_units() {
                return RiskDecision::Rejected(RiskReason::InconsistentPortfolioExposure);
            }
            portfolio.gross_exposure().minor_units() - notional.minor_units()
        }
    };
    if projected > limits.max_gross_exposure.minor_units() {
        return RiskDecision::Rejected(RiskReason::GrossExposureLimit {
            projected, maximum: limits.max_gross_exposure.minor_units(),
        });
    }
    RiskDecision::Approved {
        order_notional: notional,
        projected_gross_exposure: Money::new(portfolio.currency().clone(), projected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(side: OrderSide, quantity: u64, position: u64)
        -> (OrderIntent, PortfolioSnapshot, RiskLimits) {
        let currency = Currency::new("USD").unwrap();
        let instrument = InstrumentId::new("ACME").unwrap();
        let positions = BTreeMap::from([(instrument.clone(), position)]);
        let portfolio = PortfolioSnapshot::new(currency.clone(),
            Money::new(currency.clone(), 10_000), positions, 0).unwrap();
        let order = OrderIntent::new(OrderId::new("order-1").unwrap(),
            StrategyId::new("strategy-1").unwrap(), instrument, side,
            Quantity::new(quantity).unwrap(), Price::new(currency.clone(), 100).unwrap());
        let limits = RiskLimits::new(Money::new(currency.clone(), 5_000),
            Money::new(currency, 12_000), 10).unwrap();
        (order, portfolio, limits)
    }

    #[test]
    fn approves_order_inside_limits() {
        let (order, portfolio, limits) = fixture(OrderSide::Buy, 10, 0);
        assert!(matches!(evaluate_order(&order, &portfolio, &limits),
            RiskDecision::Approved { .. }));
    }

    #[test]
    fn rejects_order_above_notional_limit() {
        let (order, portfolio, limits) = fixture(OrderSide::Buy, 51, 0);
        assert!(matches!(evaluate_order(&order, &portfolio, &limits),
            RiskDecision::Rejected(RiskReason::OrderNotionalLimit { .. })));
    }

    #[test]
    fn rejects_sell_that_would_create_a_short_position() {
        let (order, portfolio, limits) = fixture(OrderSide::Sell, 2, 1);
        assert_eq!(evaluate_order(&order, &portfolio, &limits),
            RiskDecision::Rejected(RiskReason::InsufficientPosition {
                requested: 2, available: 1,
            }));
    }

    #[test]
    fn rejects_gross_exposure_breach() {
        let (order, mut portfolio, limits) = fixture(OrderSide::Buy, 30, 0);
        portfolio.gross_exposure = Money::new(Currency::new("USD").unwrap(), 10_000);
        assert_eq!(evaluate_order(&order, &portfolio, &limits),
            RiskDecision::Rejected(RiskReason::GrossExposureLimit {
                projected: 13_000, maximum: 12_000,
            }));
    }

    #[test]
    fn rejects_notional_overflow() {
        let currency = Currency::new("USD").unwrap();
        let order = OrderIntent::new(OrderId::new("overflow").unwrap(),
            StrategyId::new("strategy").unwrap(), InstrumentId::new("ACME").unwrap(),
            OrderSide::Buy, Quantity::new(u64::MAX).unwrap(),
            Price::new(currency.clone(), 2).unwrap());
        let portfolio = PortfolioSnapshot::new(currency.clone(),
            Money::new(currency.clone(), 0), BTreeMap::new(), 0).unwrap();
        let limits = RiskLimits::new(Money::new(currency.clone(), u64::MAX),
            Money::new(currency, u64::MAX), 1).unwrap();
        assert_eq!(evaluate_order(&order, &portfolio, &limits),
            RiskDecision::Rejected(RiskReason::ArithmeticOverflow));
    }

    #[test]
    fn rejects_currency_code_with_invalid_shape() {
        assert_eq!(Currency::new("usd"), Err(DomainError::InvalidCurrency));
    }
}
