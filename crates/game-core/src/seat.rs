//! Seats: the only notion of "player" a game engine knows about.
//!
//! Engines never see cogling identities. The platform maps seats to coglings (and
//! randomizes that mapping), which keeps engines independent of hosting.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum number of seats any game may have.
///
/// Bounded so that [`ActiveSeats`] can be a single `u64` bitmask.
pub const MAX_SEATS: u8 = 64;

/// Error returned when a seat index or seat count is out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SeatError {
    /// A seat index was `>= MAX_SEATS`.
    #[error("seat index {0} is out of range (max {max})", max = MAX_SEATS - 1)]
    IndexOutOfRange(u8),
    /// A seat count was zero or `> MAX_SEATS`.
    #[error("seat count {0} is out of range (1..={MAX_SEATS})")]
    CountOutOfRange(u8),
}

/// A zero-based seat at the table. Always `< MAX_SEATS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "u8", into = "u8")]
pub struct Seat(u8);

impl Seat {
    /// The first seat (index 0).
    pub const FIRST: Self = Self(0);
    /// The second seat (index 1).
    pub const SECOND: Self = Self(1);

    /// Creates a seat from its zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`SeatError::IndexOutOfRange`] if `index >= MAX_SEATS`.
    pub const fn new(index: u8) -> Result<Self, SeatError> {
        if index < MAX_SEATS {
            Ok(Self(index))
        } else {
            Err(SeatError::IndexOutOfRange(index))
        }
    }

    /// The zero-based index as a `u8`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The zero-based index, convenient for slice indexing.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The next seat in clockwise order, wrapping around `count`.
    #[must_use]
    pub const fn next(self, count: SeatCount) -> Self {
        Self((self.0 + 1) % count.get())
    }
}

impl TryFrom<u8> for Seat {
    type Error = SeatError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Seat> for u8 {
    fn from(seat: Seat) -> Self {
        seat.0
    }
}

impl fmt::Display for Seat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seat {}", self.0)
    }
}

/// Number of seats in a game: `1..=MAX_SEATS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "u8", into = "u8")]
pub struct SeatCount(u8);

impl SeatCount {
    /// Two seats; the most common case.
    pub const TWO: Self = Self(2);

    /// Creates a seat count.
    ///
    /// # Errors
    ///
    /// Returns [`SeatError::CountOutOfRange`] if `count` is 0 or `> MAX_SEATS`.
    pub const fn new(count: u8) -> Result<Self, SeatError> {
        if count >= 1 && count <= MAX_SEATS {
            Ok(Self(count))
        } else {
            Err(SeatError::CountOutOfRange(count))
        }
    }

    /// The count as a `u8`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The count as a `usize`.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0 as usize
    }

    /// Always `false`; a seat count is never zero. Provided for API symmetry.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Whether `seat` exists in a game with this many seats.
    #[must_use]
    pub const fn contains(self, seat: Seat) -> bool {
        seat.0 < self.0
    }

    /// Iterates over every seat in order.
    pub fn seats(self) -> impl DoubleEndedIterator<Item = Seat> + ExactSizeIterator {
        (0..self.0).map(Seat)
    }
}

impl TryFrom<u8> for SeatCount {
    type Error = SeatError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SeatCount> for u8 {
    fn from(count: SeatCount) -> Self {
        count.0
    }
}

impl fmt::Display for SeatCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Inclusive range of seat counts a game supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SeatRange {
    /// Minimum seat count.
    pub min: u8,
    /// Maximum seat count.
    pub max: u8,
}

impl SeatRange {
    /// A range that allows exactly `n` seats.
    #[must_use]
    pub const fn exactly(n: u8) -> Self {
        Self { min: n, max: n }
    }

    /// Whether `count` is within the range.
    #[must_use]
    pub const fn contains(self, count: SeatCount) -> bool {
        count.0 >= self.min && count.0 <= self.max
    }
}

/// A non-empty set of seats that must act.
///
/// Non-emptiness is guaranteed by construction, so an engine cannot report
/// "the game is running but nobody can move".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActiveSeats(u64);

impl ActiveSeats {
    /// A set containing exactly one seat (sequential games).
    #[must_use]
    pub const fn one(seat: Seat) -> Self {
        Self(1 << seat.0)
    }

    /// Builds a set from seats; returns `None` if the iterator is empty.
    pub fn from_seats(seats: impl IntoIterator<Item = Seat>) -> Option<Self> {
        let mask = seats.into_iter().fold(0_u64, |m, s| m | (1 << s.0));
        (mask != 0).then_some(Self(mask))
    }

    /// Whether `seat` must act.
    #[must_use]
    pub const fn contains(self, seat: Seat) -> bool {
        self.0 & (1 << seat.0) != 0
    }

    /// Number of seats that must act (always `>= 1`).
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Always `false`; provided for API symmetry.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Iterates over the active seats in ascending order.
    pub fn iter(self) -> impl Iterator<Item = Seat> {
        let mask = self.0;
        (0..MAX_SEATS)
            .filter(move |i| mask & (1 << i) != 0)
            .map(Seat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_bounds() {
        assert!(Seat::new(63).is_ok());
        assert_eq!(Seat::new(64), Err(SeatError::IndexOutOfRange(64)));
        assert_eq!(SeatCount::new(0), Err(SeatError::CountOutOfRange(0)));
        assert_eq!(SeatCount::new(65), Err(SeatError::CountOutOfRange(65)));
        assert!(SeatCount::new(64).is_ok());
    }

    #[test]
    fn seat_next_wraps() {
        let three = SeatCount::new(3).unwrap();
        let s2 = Seat::new(2).unwrap();
        assert_eq!(s2.next(three), Seat::FIRST);
        assert_eq!(Seat::FIRST.next(three), Seat::SECOND);
    }

    #[test]
    fn seat_serde_rejects_out_of_range() {
        assert_eq!(serde_json::from_str::<Seat>("3").unwrap().get(), 3);
        assert!(serde_json::from_str::<Seat>("64").is_err());
        assert!(serde_json::from_str::<SeatCount>("0").is_err());
        assert_eq!(serde_json::to_string(&Seat::SECOND).unwrap(), "1");
    }

    #[test]
    fn active_seats() {
        assert!(ActiveSeats::from_seats([]).is_none());
        let set = ActiveSeats::from_seats([Seat::new(5).unwrap(), Seat::FIRST]).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(Seat::FIRST));
        assert!(!set.contains(Seat::SECOND));
        assert_eq!(set.iter().map(Seat::get).collect::<Vec<_>>(), [0, 5]);
        let max = ActiveSeats::one(Seat::new(63).unwrap());
        assert_eq!(max.iter().map(Seat::get).collect::<Vec<_>>(), [63]);
    }

    #[test]
    fn seat_range() {
        let r = SeatRange { min: 2, max: 6 };
        assert!(r.contains(SeatCount::TWO));
        assert!(!r.contains(SeatCount::new(7).unwrap()));
        assert!(SeatRange::exactly(2).contains(SeatCount::TWO));
    }
}
