#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Validated market observations, source provenance, freshness checks and
//! deterministic in-memory replay.
//!
//! This crate deliberately has no network, broker, clock, database or model
//! dependency. Adapters must supply timestamps and normalized observations.
//! Prices and quantities use non-negative fixed-point integers; floats are
//! intentionally excluded from the ingestion boundary.

use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Maximum supported decimal scale.
pub const MAX_SCALE: u8 = 18;

/// Errors raised while constructing market-data values.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MarketDataError {
    /// An identifier is empty, too long or contains unsupported characters.
    #[error("invalid {kind} identifier: {value}")]
    InvalidIdentifier { kind: &'static str, value: String },
    /// A fixed-point scale exceeds the supported maximum.
    #[error("decimal scale {scale} exceeds maximum {maximum}")]
    InvalidScale { scale: u8, maximum: u8 },
    /// A required positive value was zero.
    #[error("{0} must be greater than zero")]
    MustBePositive(&'static str),
    /// Values being compared use different decimal scales.
    #[error("compared values must use the same decimal scale")]
    ScaleMismatch,
    /// A quote has a bid greater than its ask.
    #[error("quote bid price exceeds ask price")]
    CrossedQuote,
    /// OHLC values do not form a valid candle range.
    #[error("candle OHLC values violate low <= open/close <= high")]
    InvalidCandleRange,
    /// An interval duration is zero.
    #[error("bar interval must be greater than zero")]
    InvalidInterval,
    /// A source sequence number is zero.
    #[error("source sequence must be greater than zero")]
    InvalidSourceSequence,
}

/// Validated instrument identifier, normally a canonical symbol or venue symbol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId(String);

/// Validated identity of the upstream feed or adapter that supplied an event.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(String);

macro_rules! validated_id {
    ($type:ident, $kind:literal, $max:expr) => {
        impl $type {
            /// Creates an identifier containing ASCII letters, digits, and
            /// the punctuation characters hyphen, underscore, dot, colon, slash.
            pub fn new(value: impl Into<String>) -> Result<Self, MarketDataError> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= $max
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
                    });
                if !valid {
                    return Err(MarketDataError::InvalidIdentifier {
                        kind: $kind,
                        value,
                    });
                }
                Ok(Self(value))
            }

            /// Returns the identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

validated_id!(InstrumentId, "instrument", 96);
validated_id!(SourceId, "source", 96);

/// Exact signed decimal represented as coefficient × 10 to the power
/// negative scale. Signed values support markets where an instrument can trade
/// at a negative price; sizes remain validated as strictly positive.
///
/// Values compared together must have the same scale. This avoids implicit
/// rounding and keeps all market-data arithmetic deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fixed {
    coefficient: i64,
    scale: u8,
}

impl Fixed {
    /// Creates a fixed-point value. A coefficient of zero is allowed; fields
    /// requiring a positive value enforce that constraint separately.
    pub fn new(coefficient: i64, scale: u8) -> Result<Self, MarketDataError> {
        if scale > MAX_SCALE {
            return Err(MarketDataError::InvalidScale {
                scale,
                maximum: MAX_SCALE,
            });
        }
        Ok(Self { coefficient, scale })
    }

    /// Returns the integer coefficient.
    #[must_use]
    pub fn coefficient(self) -> i64 {
        self.coefficient
    }

    /// Returns the number of decimal places.
    #[must_use]
    pub fn scale(self) -> u8 {
        self.scale
    }

    /// Compares values without rounding or implicit scale conversion.
    pub fn checked_cmp(self, other: Self) -> Result<std::cmp::Ordering, MarketDataError> {
        if self.scale != other.scale {
            return Err(MarketDataError::ScaleMismatch);
        }
        Ok(self.coefficient.cmp(&other.coefficient))
    }
}

/// Unix timestamp in nanoseconds, supplied by the feed adapter.
///
/// The crate does not read a system clock or infer exchange time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimestampNanos(u64);

impl TimestampNanos {
    /// Creates a timestamp from Unix-epoch nanoseconds.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns Unix-epoch nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Returns the elapsed nanoseconds from an earlier timestamp, if ordered.
    #[must_use]
    pub const fn elapsed_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }
}

/// Positive duration in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DurationNanos(u64);

impl DurationNanos {
    /// Creates a strictly positive duration.
    pub fn new(nanos: u64) -> Result<Self, MarketDataError> {
        if nanos == 0 {
            return Err(MarketDataError::MustBePositive("duration"));
        }
        Ok(Self(nanos))
    }

    /// Returns the duration in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }
}

/// Trade observation from a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trade {
    price: Fixed,
    quantity: Fixed,
}

impl Trade {
    /// Creates a trade with strictly positive price and quantity.
    pub fn new(price: Fixed, quantity: Fixed) -> Result<Self, MarketDataError> {
        if !quantity.is_positive() {
            return Err(MarketDataError::MustBePositive("trade quantity"));
        }
        Ok(Self { price, quantity })
    }

    /// Returns the trade price.
    #[must_use]
    pub const fn price(self) -> Fixed {
        self.price
    }

    /// Returns the traded quantity.
    #[must_use]
    pub const fn quantity(self) -> Fixed {
        self.quantity
    }
}

/// Best bid/ask observation. Bid and ask prices must share a scale, as must sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    bid_price: Fixed,
    ask_price: Fixed,
    bid_size: Fixed,
    ask_size: Fixed,
}

impl Quote {
    /// Creates a non-crossed quote with positive prices and displayed sizes.
    pub fn new(
        bid_price: Fixed,
        ask_price: Fixed,
        bid_size: Fixed,
        ask_size: Fixed,
    ) -> Result<Self, MarketDataError> {
        if !bid_size.is_positive() || !ask_size.is_positive() {
            return Err(MarketDataError::MustBePositive("quote size"));
        }
        if bid_price.checked_cmp(ask_price)? == std::cmp::Ordering::Greater {
            return Err(MarketDataError::CrossedQuote);
        }
        if bid_size.scale() != ask_size.scale() {
            return Err(MarketDataError::ScaleMismatch);
        }
        Ok(Self {
            bid_price,
            ask_price,
            bid_size,
            ask_size,
        })
    }

    /// Returns the best bid price.
    #[must_use]
    pub const fn bid_price(self) -> Fixed {
        self.bid_price
    }

    /// Returns the best ask price.
    #[must_use]
    pub const fn ask_price(self) -> Fixed {
        self.ask_price
    }

    /// Returns displayed bid size.
    #[must_use]
    pub const fn bid_size(self) -> Fixed {
        self.bid_size
    }

    /// Returns displayed ask size.
    #[must_use]
    pub const fn ask_size(self) -> Fixed {
        self.ask_size
    }
}

/// Candle interval duration. Candle timestamps identify the interval start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BarInterval(DurationNanos);

impl BarInterval {
    /// Creates a non-zero candle interval.
    pub fn new(nanos: u64) -> Result<Self, MarketDataError> {
        Ok(Self(DurationNanos::new(nanos)?))
    }

    /// Returns interval duration in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0.as_nanos()
    }
}

/// OHLCV bar. OHLC prices share one scale; volume is independently scaled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candle {
    interval: BarInterval,
    open: Fixed,
    high: Fixed,
    low: Fixed,
    close: Fixed,
    volume: Fixed,
}

impl Candle {
    /// Creates a validated candle with a coherent OHLC range.
    pub fn new(
        interval: BarInterval,
        open: Fixed,
        high: Fixed,
        low: Fixed,
        close: Fixed,
        volume: Fixed,
    ) -> Result<Self, MarketDataError> {
        if open.scale() != high.scale()
            || open.scale() != low.scale()
            || open.scale() != close.scale()
        {
            return Err(MarketDataError::ScaleMismatch);
        }
        let low_below_open = low.checked_cmp(open)? != std::cmp::Ordering::Greater;
        let low_below_close = low.checked_cmp(close)? != std::cmp::Ordering::Greater;
        let high_above_open = high.checked_cmp(open)? != std::cmp::Ordering::Less;
        let high_above_close = high.checked_cmp(close)? != std::cmp::Ordering::Less;
        if !(low_below_open && low_below_close && high_above_open && high_above_close) {
            return Err(MarketDataError::InvalidCandleRange);
        }
        Ok(Self {
            interval,
            open,
            high,
            low,
            close,
            volume,
        })
    }

    /// Returns candle interval.
    #[must_use]
    pub const fn interval(self) -> BarInterval {
        self.interval
    }

    /// Returns opening price.
    #[must_use]
    pub const fn open(self) -> Fixed {
        self.open
    }

    /// Returns high price.
    #[must_use]
    pub const fn high(self) -> Fixed {
        self.high
    }

    /// Returns low price.
    #[must_use]
    pub const fn low(self) -> Fixed {
        self.low
    }

    /// Returns closing price.
    #[must_use]
    pub const fn close(self) -> Fixed {
        self.close
    }

    /// Returns traded volume.
    #[must_use]
    pub const fn volume(self) -> Fixed {
        self.volume
    }
}

/// Normalized market observation payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketPayload {
    /// Individual trade print.
    Trade(Trade),
    /// Best bid and ask.
    Quote(Quote),
    /// OHLCV candle.
    Candle(Candle),
}

/// A normalized market event with event-time, receive-time and source provenance.
///
/// Event time is the source's claimed market/event timestamp. Received-at is
/// when the adapter received it. The two are intentionally never conflated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketEvent {
    instrument: InstrumentId,
    source: SourceId,
    source_sequence: u64,
    event_time: TimestampNanos,
    received_at: TimestampNanos,
    payload: MarketPayload,
}

impl MarketEvent {
    /// Creates an event with explicit source sequence and both timestamps.
    pub fn new(
        instrument: InstrumentId,
        source: SourceId,
        source_sequence: u64,
        event_time: TimestampNanos,
        received_at: TimestampNanos,
        payload: MarketPayload,
    ) -> Result<Self, MarketDataError> {
        if source_sequence == 0 {
            return Err(MarketDataError::InvalidSourceSequence);
        }
        Ok(Self {
            instrument,
            source,
            source_sequence,
            event_time,
            received_at,
            payload,
        })
    }

    /// Returns the instrument.
    #[must_use]
    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    /// Returns the supplying source.
    #[must_use]
    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// Returns the source-local sequence number.
    #[must_use]
    pub const fn source_sequence(&self) -> u64 {
        self.source_sequence
    }

    /// Returns source event time.
    #[must_use]
    pub const fn event_time(&self) -> TimestampNanos {
        self.event_time
    }

    /// Returns adapter receive time.
    #[must_use]
    pub const fn received_at(&self) -> TimestampNanos {
        self.received_at
    }

    /// Returns the normalized payload.
    #[must_use]
    pub const fn payload(&self) -> MarketPayload {
        self.payload
    }
}

/// Freshness result relative to a caller-supplied current timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Event time is not in the future and age is within the configured bound.
    Fresh,
    /// Event is older than the configured maximum age.
    Stale,
    /// Event time is later than the caller-supplied current time.
    FutureDated,
}

/// Evaluates freshness using event time, not receive time.
///
/// Callers must supply a trusted now value and an explicit maximum age.
#[must_use]
pub fn assess_freshness(
    event: &MarketEvent,
    now: TimestampNanos,
    max_age: DurationNanos,
) -> Freshness {
    let Some(age) = now.elapsed_since(event.event_time()) else {
        return Freshness::FutureDated;
    };
    if age > max_age.as_nanos() {
        Freshness::Stale
    } else {
        Freshness::Fresh
    }
}

/// Half-open replay interval, including start and excluding end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayWindow {
    start: TimestampNanos,
    end: TimestampNanos,
}

impl ReplayWindow {
    /// Creates a non-empty replay window.
    pub fn new(
        start: TimestampNanos,
        end: TimestampNanos,
    ) -> Result<Self, MarketDataError> {
        if start >= end {
            return Err(MarketDataError::InvalidInterval);
        }
        Ok(Self { start, end })
    }

    /// Returns the inclusive start timestamp.
    #[must_use]
    pub const fn start(self) -> TimestampNanos {
        self.start
    }

    /// Returns the exclusive end timestamp.
    #[must_use]
    pub const fn end(self) -> TimestampNanos {
        self.end
    }

    fn contains(self, timestamp: TimestampNanos) -> bool {
        timestamp >= self.start && timestamp < self.end
    }
}

/// Deterministic replay ordering: event time, source ID, then source sequence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReplayKey {
    event_time: TimestampNanos,
    source: SourceId,
    source_sequence: u64,
}

impl From<&MarketEvent> for ReplayKey {
    fn from(event: &MarketEvent) -> Self {
        Self {
            event_time: event.event_time,
            source: event.source.clone(),
            source_sequence: event.source_sequence,
        }
    }
}

/// Journal errors.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum JournalError {
    /// The same source-local sequence was already ingested.
    #[error("duplicate source sequence {sequence} from {source}")]
    DuplicateSourceSequence {
        /// Source identifier.
        source: String,
        /// Repeated source sequence.
        sequence: u64,
    },
}

/// Append-only in-memory event journal with deterministic replay.
///
/// This is a reference implementation, not durable storage. A persistent
/// journal and raw-message archive are later integration work.
#[derive(Clone, Debug, Default)]
pub struct MarketJournal {
    events: BTreeMap<ReplayKey, MarketEvent>,
    seen_sequences: BTreeSet<(SourceId, u64)>,
}

impl MarketJournal {
    /// Creates an empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an event, rejecting a repeated source/sequence pair.
    ///
    /// Out-of-order event timestamps are accepted: feeds can deliver late
    /// observations. Replay sorts by event time rather than arrival order.
    pub fn append(&mut self, event: MarketEvent) -> Result<(), JournalError> {
        let sequence_key = (event.source.clone(), event.source_sequence);
        if self.seen_sequences.contains(&sequence_key) {
            return Err(JournalError::DuplicateSourceSequence {
                source: event.source.as_str().to_owned(),
                sequence: event.source_sequence,
            });
        }
        let replay_key = ReplayKey::from(&event);
        self.seen_sequences.insert(sequence_key);
        self.events.insert(replay_key, event);
        Ok(())
    }

    /// Returns the number of journaled events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns whether the journal contains no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Replays all instruments in deterministic event-time order.
    #[must_use]
    pub fn replay(&self, window: ReplayWindow) -> Vec<&MarketEvent> {
        self.events
            .iter()
            .filter(|(key, _)| window.contains(key.event_time))
            .map(|(_, event)| event)
            .collect()
    }

    /// Replays one instrument in deterministic event-time order.
    #[must_use]
    pub fn replay_instrument(
        &self,
        instrument: &InstrumentId,
        window: ReplayWindow,
    ) -> Vec<&MarketEvent> {
        self.replay(window)
            .into_iter()
            .filter(|event| event.instrument() == instrument)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(value: i64, scale: u8) -> Fixed {
        Fixed::new(value, scale).unwrap()
    }

    fn trade_event(
        source: &str,
        sequence: u64,
        event_time: u64,
        received_at: u64,
    ) -> MarketEvent {
        MarketEvent::new(
            InstrumentId::new("NASDAQ/AAPL").unwrap(),
            SourceId::new(source).unwrap(),
            sequence,
            TimestampNanos::new(event_time),
            TimestampNanos::new(received_at),
            MarketPayload::Trade(Trade::new(fixed(15025, 2), fixed(10, 0)).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn rejects_invalid_identifier() {
        assert!(InstrumentId::new("bad symbol").is_err());
    }

    #[test]
    fn rejects_scale_above_supported_precision() {
        assert_eq!(
            Fixed::new(1, MAX_SCALE + 1),
            Err(MarketDataError::InvalidScale {
                scale: MAX_SCALE + 1,
                maximum: MAX_SCALE,
            })
        );
    }

    #[test]
    fn accepts_negative_market_prices_but_requires_positive_quantity() {
        let trade = Trade::new(fixed(-125, 2), fixed(1, 0)).unwrap();
        assert_eq!(trade.price().coefficient(), -125);
        assert_eq!(
            Trade::new(fixed(125, 2), fixed(0, 0)),
            Err(MarketDataError::MustBePositive("trade quantity"))
        );
    }

    #[test]
    fn rejects_crossed_quote() {
        assert_eq!(
            Quote::new(fixed(102, 2), fixed(101, 2), fixed(1, 0), fixed(1, 0)),
            Err(MarketDataError::CrossedQuote)
        );
    }

    #[test]
    fn rejects_quote_scale_mismatch() {
        assert_eq!(
            Quote::new(fixed(100, 2), fixed(101, 3), fixed(1, 0), fixed(1, 0)),
            Err(MarketDataError::ScaleMismatch)
        );
    }

    #[test]
    fn rejects_invalid_candle_range() {
        let interval = BarInterval::new(60_000_000_000).unwrap();
        assert_eq!(
            Candle::new(
                interval,
                fixed(100, 2),
                fixed(99, 2),
                fixed(98, 2),
                fixed(101, 2),
                fixed(1000, 0),
            ),
            Err(MarketDataError::InvalidCandleRange)
        );
    }

    #[test]
    fn freshness_uses_event_time_and_detects_future_events() {
        let event = trade_event("feed-a", 1, 100, 150);
        let max_age = DurationNanos::new(50).unwrap();
        assert_eq!(
            assess_freshness(&event, TimestampNanos::new(150), max_age),
            Freshness::Fresh
        );
        assert_eq!(
            assess_freshness(&event, TimestampNanos::new(151), max_age),
            Freshness::Stale
        );
        assert_eq!(
            assess_freshness(&event, TimestampNanos::new(99), max_age),
            Freshness::FutureDated
        );
    }

    #[test]
    fn journal_replays_by_event_time_not_arrival_order() {
        let mut journal = MarketJournal::new();
        journal.append(trade_event("feed-a", 2, 200, 300)).unwrap();
        journal.append(trade_event("feed-a", 1, 100, 350)).unwrap();
        let window = ReplayWindow::new(TimestampNanos::new(100), TimestampNanos::new(300))
            .unwrap();
        let replay = journal.replay(window);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].event_time().as_nanos(), 100);
        assert_eq!(replay[1].event_time().as_nanos(), 200);
    }

    #[test]
    fn journal_rejects_duplicate_source_sequence() {
        let mut journal = MarketJournal::new();
        journal.append(trade_event("feed-a", 7, 100, 100)).unwrap();
        assert_eq!(
            journal.append(trade_event("feed-a", 7, 101, 101)),
            Err(JournalError::DuplicateSourceSequence {
                source: "feed-a".to_owned(),
                sequence: 7,
            })
        );
        assert_eq!(journal.len(), 1);
    }

    #[test]
    fn replay_window_is_half_open_and_instrument_filter_works() {
        let mut journal = MarketJournal::new();
        journal.append(trade_event("feed-a", 1, 100, 100)).unwrap();
        journal.append(trade_event("feed-a", 2, 200, 200)).unwrap();
        let window = ReplayWindow::new(TimestampNanos::new(100), TimestampNanos::new(200))
            .unwrap();
        let instrument = InstrumentId::new("NASDAQ/AAPL").unwrap();
        let replay = journal.replay_instrument(&instrument, window);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].source_sequence(), 1);
    }

    #[test]
    fn event_and_receive_timestamps_remain_distinct() {
        let event = trade_event("feed-a", 1, 100, 150);
        assert_eq!(event.event_time().as_nanos(), 100);
        assert_eq!(event.received_at().as_nanos(), 150);
    }
}
