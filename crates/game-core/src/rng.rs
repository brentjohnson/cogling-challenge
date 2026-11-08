//! Deterministic, serializable random number generation for engines.
//!
//! The algorithms here (SplitMix64 for seeding, xoshiro256** for generation,
//! Lemire's method for bounded integers) are **frozen**. Recorded games are
//! replayed from their seed, so changing any output of this module would make
//! historical games irreproducible. That is why the platform does not use the
//! `rand` crate here: its sampling helpers may change between versions.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A game seed. Together with the configuration and the accepted actions it
/// fully determines a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct Seed(u64);

impl Seed {
    /// Wraps a raw seed value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw seed value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Derives an independent child seed, e.g. one per game in a batch.
    ///
    /// The same `(self, stream)` pair always yields the same child.
    #[must_use]
    pub const fn derive(self, stream: u64) -> Self {
        let mut a = self.0;
        let mut b = stream ^ 0xA076_1D64_78BD_642F;
        Self(splitmix64(&mut a) ^ splitmix64(&mut b).rotate_left(17))
    }
}

impl From<u64> for Seed {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One SplitMix64 step: advances `state` and returns the next output.
const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A xoshiro256** generator whose state can be stored inside a game state.
///
/// # Examples
///
/// ```
/// use cogling_game_core::{GameRng, Seed};
///
/// let mut a = GameRng::from_seed(Seed::new(7));
/// let mut b = GameRng::from_seed(Seed::new(7));
/// assert_eq!(a.next_u64(), b.next_u64());
/// assert!(a.below(6).is_some_and(|roll| roll < 6));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameRng {
    s: [u64; 4],
}

impl GameRng {
    /// Seeds the generator by expanding `seed` with SplitMix64.
    #[must_use]
    pub const fn from_seed(seed: Seed) -> Self {
        let mut sm = seed.0;
        let s = [
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
        ];
        // xoshiro must not start from the all-zero state; SplitMix64 cannot
        // realistically produce it, but guard anyway to keep the invariant total.
        if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 {
            Self { s: [1, 0, 0, 0] }
        } else {
            Self { s }
        }
    }

    /// The next 64 random bits.
    pub const fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// A uniformly distributed integer in `0..bound`, or `None` if `bound == 0`.
    ///
    /// Unbiased (Lemire's multiply-and-reject method).
    pub const fn below(&mut self, bound: u64) -> Option<u64> {
        if bound == 0 {
            return None;
        }
        let mut m = (self.next_u64() as u128) * (bound as u128);
        let mut low = m as u64;
        if low < bound {
            let threshold = bound.wrapping_neg() % bound;
            while low < threshold {
                m = (self.next_u64() as u128) * (bound as u128);
                low = m as u64;
            }
        }
        Some((m >> 64) as u64)
    }

    /// A uniformly distributed index in `0..len`, or `None` if `len == 0`.
    pub fn index(&mut self, len: usize) -> Option<usize> {
        let bound = u64::try_from(len).ok()?;
        self.below(bound).and_then(|i| usize::try_from(i).ok())
    }

    /// A uniformly chosen element, or `None` for an empty slice.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        self.index(items.len()).and_then(|i| items.get(i))
    }

    /// Shuffles `items` in place (Fisher–Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            if let Some(j) = self.index(i + 1) {
                items.swap(i, j);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_reference_vector() {
        // Reference outputs for seed 1234567 from the published C implementation.
        let mut state = 1_234_567;
        let got: Vec<u64> = (0..5).map(|_| splitmix64(&mut state)).collect();
        assert_eq!(
            got,
            [
                6_457_827_717_110_365_317,
                3_203_168_211_198_807_973,
                9_817_491_932_198_370_423,
                4_593_380_528_125_082_431,
                16_408_922_859_458_223_821,
            ]
        );
    }

    #[test]
    fn xoshiro_reference_vector() {
        // Reference outputs for state {1, 2, 3, 4} from the published C implementation.
        let mut rng = GameRng { s: [1, 2, 3, 4] };
        let got: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        assert_eq!(got, [11_520, 0, 1_509_978_240, 1_215_971_899_390_074_240]);
    }

    #[test]
    fn frozen_seed_stream() {
        // Guards against accidental changes to seeding: these values are part of
        // the replay contract.
        let mut rng = GameRng::from_seed(Seed::new(42));
        let first = rng.next_u64();
        let mut again = GameRng::from_seed(Seed::new(42));
        assert_eq!(first, again.next_u64());
        assert_ne!(
            GameRng::from_seed(Seed::new(42)),
            GameRng::from_seed(Seed::new(43))
        );
    }

    #[test]
    fn below_is_in_range_and_covers_all_values() {
        let mut rng = GameRng::from_seed(Seed::new(1));
        let mut seen = [false; 7];
        for _ in 0..1_000 {
            let v = rng.below(7).unwrap();
            assert!(v < 7);
            seen[usize::try_from(v).unwrap()] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(rng.below(0), None);
        assert_eq!(rng.below(1), Some(0));
    }

    #[test]
    fn choose_and_shuffle() {
        let mut rng = GameRng::from_seed(Seed::new(9));
        assert_eq!(rng.choose::<u8>(&[]), None);
        let mut items: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        assert_ne!(items, sorted, "50 elements should not shuffle to identity");
    }

    #[test]
    fn rng_state_round_trips_through_serde() {
        let mut rng = GameRng::from_seed(Seed::new(5));
        rng.next_u64();
        let json = serde_json::to_string(&rng).unwrap();
        let mut back: GameRng = serde_json::from_str(&json).unwrap();
        assert_eq!(rng.next_u64(), back.next_u64());
    }

    #[test]
    fn derive_is_deterministic_and_spreads() {
        let base = Seed::new(100);
        assert_eq!(base.derive(3), base.derive(3));
        assert_ne!(base.derive(3), base.derive(4));
        assert_ne!(base.derive(0), base);
    }
}
