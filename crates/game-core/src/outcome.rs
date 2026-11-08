//! Game status, engine outcomes, and platform-level results.

use std::fmt;
use std::num::NonZeroU8;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::seat::{ActiveSeats, Seat, SeatCount};

/// A finishing position: 1 is best. Ties share a rank.
///
/// Ranks follow *standard competition ranking* ("1224"): a seat's rank is one
/// plus the number of seats that finished strictly ahead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "u8", into = "u8")]
pub struct Rank(NonZeroU8);

impl Rank {
    /// First place.
    pub const FIRST: Self = Self(NonZeroU8::MIN);

    /// Creates a rank; `None` for zero.
    #[must_use]
    pub const fn new(rank: u8) -> Option<Self> {
        match NonZeroU8::new(rank) {
            Some(r) => Some(Self(r)),
            None => None,
        }
    }

    /// The rank as a `u8` (always `>= 1`).
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }
}

impl TryFrom<u8> for Rank {
    type Error = OutcomeError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(OutcomeError::ZeroRank)
    }
}

impl From<Rank> for u8 {
    fn from(rank: Rank) -> Self {
        rank.get()
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

/// Errors constructing an [`Outcome`] or ranks.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum OutcomeError {
    /// A rank of zero was supplied.
    #[error("rank must be >= 1")]
    ZeroRank,
    /// The number of ranks did not match the seat count.
    #[error("expected {expected} ranks, got {got}")]
    WrongLength {
        /// Seats in the game.
        expected: usize,
        /// Ranks supplied.
        got: usize,
    },
    /// Ranks are not a valid standard competition ranking.
    #[error("ranks {0:?} are not a standard competition ranking")]
    Inconsistent(Vec<u8>),
}

/// Computes standard competition ranks from per-seat keys where **higher is better**.
///
/// # Examples
///
/// ```
/// use cogling_game_core::ranks_from_keys;
///
/// let ranks: Vec<u8> = ranks_from_keys(&[10, 30, 30, 5]).iter().map(|r| r.get()).collect();
/// assert_eq!(ranks, [3, 1, 1, 4]);
/// ```
#[must_use]
pub fn ranks_from_keys<K: Ord>(keys: &[K]) -> Vec<Rank> {
    keys.iter()
        .map(|k| {
            let ahead = keys.iter().filter(|other| *other > k).count();
            // `ahead < keys.len() <= MAX_SEATS (64)`, so `ahead + 1` fits in a u8
            // and is non-zero; saturate defensively rather than panic.
            let rank = u8::try_from(ahead + 1).unwrap_or(u8::MAX);
            Rank::new(rank).unwrap_or(Rank::FIRST)
        })
        .collect()
}

fn check_ranks(ranks: &[Rank], seats: SeatCount) -> Result<(), OutcomeError> {
    if ranks.len() != seats.len() {
        return Err(OutcomeError::WrongLength {
            expected: seats.len(),
            got: ranks.len(),
        });
    }
    let consistent = ranks.iter().all(|r| {
        let ahead = ranks.iter().filter(|o| *o < r).count();
        usize::from(r.get()) == ahead + 1
    });
    if consistent {
        Ok(())
    } else {
        Err(OutcomeError::Inconsistent(
            ranks.iter().map(|r| r.get()).collect(),
        ))
    }
}

/// The authoritative result of a finished game, as determined by the engine.
///
/// `S` is the game-specific score detail (e.g. victory points, winning line),
/// which the platform preserves verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome<S> {
    ranks: Vec<Rank>,
    scores: S,
}

impl<S> Outcome<S> {
    /// Builds an outcome from explicit ranks.
    ///
    /// # Errors
    ///
    /// Fails if `ranks.len() != seats` or the ranks are not a standard
    /// competition ranking.
    pub fn from_ranks(ranks: Vec<Rank>, seats: SeatCount, scores: S) -> Result<Self, OutcomeError> {
        check_ranks(&ranks, seats)?;
        Ok(Self { ranks, scores })
    }

    /// Builds an outcome from per-seat keys where higher is better.
    #[must_use]
    pub fn from_keys<K: Ord>(keys: &[K], scores: S) -> Self {
        Self {
            ranks: ranks_from_keys(keys),
            scores,
        }
    }

    /// `winner` ranks first; everyone else shares second place.
    #[must_use]
    pub fn winner(winner: Seat, seats: SeatCount, scores: S) -> Self {
        let keys: Vec<bool> = seats.seats().map(|s| s == winner).collect();
        Self::from_keys(&keys, scores)
    }

    /// Everyone shares first place.
    #[must_use]
    pub fn draw(seats: SeatCount, scores: S) -> Self {
        Self {
            ranks: vec![Rank::FIRST; seats.len()],
            scores,
        }
    }

    /// Per-seat ranks, indexed by seat.
    #[must_use]
    pub fn ranks(&self) -> &[Rank] {
        &self.ranks
    }

    /// Game-specific score detail.
    #[must_use]
    pub fn scores(&self) -> &S {
        &self.scores
    }

    /// Seats that finished first.
    pub fn winners(&self) -> impl Iterator<Item = Seat> + '_ {
        self.ranks
            .iter()
            .zip(0_u8..)
            .filter(|(r, _)| **r == Rank::FIRST)
            .filter_map(|(_, i)| Seat::new(i).ok())
    }

    /// Transforms the score detail, keeping ranks.
    pub fn map_scores<T>(self, f: impl FnOnce(S) -> T) -> Outcome<T> {
        Outcome {
            ranks: self.ranks,
            scores: f(self.scores),
        }
    }

    /// Like [`Outcome::map_scores`] but fallible.
    ///
    /// # Errors
    ///
    /// Propagates the error returned by `f`.
    pub fn try_map_scores<T, E>(self, f: impl FnOnce(S) -> Result<T, E>) -> Result<Outcome<T>, E> {
        Ok(Outcome {
            ranks: self.ranks,
            scores: f(self.scores)?,
        })
    }

    /// Splits into `(ranks, scores)`.
    #[must_use]
    pub fn into_parts(self) -> (Vec<Rank>, S) {
        (self.ranks, self.scores)
    }
}

/// What the game needs next.
///
/// A finished game cannot have pending actors and a running game always has
/// at least one, so neither impossible state can be represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status<S> {
    /// The listed seats must act. For sequential games this is one seat.
    ToAct(ActiveSeats),
    /// The game is over.
    Finished(Outcome<S>),
}

impl<S> Status<S> {
    /// Whether the game is over.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self, Self::Finished(_))
    }
}

/// Why a seat forfeited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForfeitReason {
    /// The decision deadline expired.
    Timeout,
    /// The cogling stayed disconnected past the reconnect grace period.
    Disconnected,
    /// An in-process agent failed to produce a legal action (simulation only;
    /// networked coglings may retry until the deadline instead).
    InvalidAction,
}

/// How the non-forfeiting seats are ranked when one seat forfeits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ForfeitPolicy {
    /// All remaining seats share first place.
    #[default]
    TiedOthers,
    /// Remaining seats are ordered by the engine's provisional ranking, if it
    /// provides one; otherwise they tie.
    Provisional,
}

/// Platform-level result of a game.
///
/// Wraps the engine's [`Outcome`] and adds results the engine knows nothing
/// about (forfeits, aborts). Scores are type-erased JSON at this level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GameResult {
    /// The game ran to completion under its rules.
    Completed {
        /// Per-seat ranks.
        ranks: Vec<Rank>,
        /// Game-specific score detail.
        scores: serde_json::Value,
    },
    /// The game ended early because a seat forfeited.
    Forfeited {
        /// The seat that forfeited (ranked last).
        seat: Seat,
        /// Why it forfeited.
        reason: ForfeitReason,
        /// Per-seat ranks after applying the forfeit policy.
        ranks: Vec<Rank>,
    },
    /// The game ended without a result (e.g. internal error, lobby expired).
    Aborted {
        /// Human-readable reason.
        reason: String,
    },
}

impl GameResult {
    /// A completed game.
    #[must_use]
    pub fn completed(outcome: Outcome<serde_json::Value>) -> Self {
        let (ranks, scores) = outcome.into_parts();
        Self::Completed { ranks, scores }
    }

    /// A forfeit by `seat`, ranking it last and the rest per `policy`.
    ///
    /// `provisional` is the engine's current standing (lower is better); it is
    /// used only with [`ForfeitPolicy::Provisional`] and only if it has one
    /// entry per seat.
    #[must_use]
    pub fn forfeit(
        seat: Seat,
        reason: ForfeitReason,
        seats: SeatCount,
        policy: ForfeitPolicy,
        provisional: Option<&[Rank]>,
    ) -> Self {
        let provisional = match policy {
            ForfeitPolicy::Provisional => provisional.filter(|p| p.len() == seats.len()),
            ForfeitPolicy::TiedOthers => None,
        };
        // Higher key is better: forfeiter strictly last, others by provisional rank.
        let keys: Vec<(bool, std::cmp::Reverse<u8>)> = seats
            .seats()
            .map(|s| {
                let standing = provisional
                    .and_then(|p| p.get(s.index()))
                    .map_or(1, |r| r.get());
                (s != seat, std::cmp::Reverse(standing))
            })
            .collect();
        Self::Forfeited {
            seat,
            reason,
            ranks: ranks_from_keys(&keys),
        }
    }

    /// Per-seat ranks, if the game produced any.
    #[must_use]
    pub fn ranks(&self) -> Option<&[Rank]> {
        match self {
            Self::Completed { ranks, .. } | Self::Forfeited { ranks, .. } => Some(ranks),
            Self::Aborted { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &[u8]) -> Vec<Rank> {
        v.iter().map(|&x| Rank::new(x).unwrap()).collect()
    }

    fn raw(ranks: &[Rank]) -> Vec<u8> {
        ranks.iter().map(|r| r.get()).collect()
    }

    #[test]
    fn rank_rejects_zero() {
        assert!(Rank::new(0).is_none());
        assert!(serde_json::from_str::<Rank>("0").is_err());
        assert_eq!(serde_json::from_str::<Rank>("2").unwrap().get(), 2);
    }

    #[test]
    fn from_keys_standard_competition() {
        assert_eq!(raw(&ranks_from_keys(&[1, 1, 1])), [1, 1, 1]);
        assert_eq!(raw(&ranks_from_keys(&[3, 2, 1])), [1, 2, 3]);
        assert_eq!(raw(&ranks_from_keys(&[5, 9, 5, 0])), [2, 1, 2, 4]);
    }

    #[test]
    fn from_ranks_validates() {
        let four = SeatCount::new(4).unwrap();
        assert!(Outcome::from_ranks(r(&[1, 2, 2, 4]), four, ()).is_ok());
        assert_eq!(
            Outcome::from_ranks(r(&[1, 2, 3, 3, 3]), four, ()),
            Err(OutcomeError::WrongLength {
                expected: 4,
                got: 5
            })
        );
        assert_eq!(
            Outcome::from_ranks(r(&[1, 2, 2, 3]), four, ()),
            Err(OutcomeError::Inconsistent(vec![1, 2, 2, 3]))
        );
        assert!(Outcome::from_ranks(r(&[2, 2]), SeatCount::TWO, ()).is_err());
    }

    #[test]
    fn winner_and_draw() {
        let three = SeatCount::new(3).unwrap();
        let w = Outcome::winner(Seat::SECOND, three, ());
        assert_eq!(raw(w.ranks()), [2, 1, 2]);
        assert_eq!(w.winners().collect::<Vec<_>>(), [Seat::SECOND]);
        let d = Outcome::draw(three, ());
        assert_eq!(raw(d.ranks()), [1, 1, 1]);
        assert_eq!(d.winners().count(), 3);
    }

    #[test]
    fn forfeit_tied_others() {
        let four = SeatCount::new(4).unwrap();
        let res = GameResult::forfeit(
            Seat::SECOND,
            ForfeitReason::Timeout,
            four,
            ForfeitPolicy::TiedOthers,
            Some(&r(&[4, 1, 2, 3])),
        );
        assert_eq!(raw(res.ranks().unwrap()), [1, 4, 1, 1]);
    }

    #[test]
    fn forfeit_provisional() {
        let four = SeatCount::new(4).unwrap();
        let res = GameResult::forfeit(
            Seat::SECOND,
            ForfeitReason::Disconnected,
            four,
            ForfeitPolicy::Provisional,
            Some(&r(&[3, 1, 2, 3])),
        );
        // Seat 1 (provisional leader) forfeits; seat 2 now leads, seats 0 and 3 tie.
        assert_eq!(raw(res.ranks().unwrap()), [2, 4, 1, 2]);
        let fallback = GameResult::forfeit(
            Seat::FIRST,
            ForfeitReason::Timeout,
            SeatCount::TWO,
            ForfeitPolicy::Provisional,
            None,
        );
        assert_eq!(raw(fallback.ranks().unwrap()), [2, 1]);
    }

    #[test]
    fn game_result_json_shape() {
        let res = GameResult::forfeit(
            Seat::FIRST,
            ForfeitReason::Timeout,
            SeatCount::TWO,
            ForfeitPolicy::TiedOthers,
            None,
        );
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind":"forfeited","seat":0,"reason":"timeout","ranks":[2,1]})
        );
        let back: GameResult = serde_json::from_value(json).unwrap();
        assert_eq!(back, res);
        assert!(GameResult::Aborted { reason: "x".into() }.ranks().is_none());
    }
}
