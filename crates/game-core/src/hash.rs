//! Stable state hashing for determinism checks.

use std::fmt;
use std::io;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 64-bit FNV-1a hash of a value's canonical JSON serialization.
///
/// Used to verify that a replay reproduces the recorded state after every
/// action. It is a determinism check, not a cryptographic digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateHash(u64);

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct FnvWriter(u64);

impl io::Write for FnvWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StateHash {
    /// Hashes the JSON serialization of `value` without allocating it.
    ///
    /// # Errors
    ///
    /// Propagates serialization failures (e.g. maps with non-string keys).
    pub fn of<T: Serialize + ?Sized>(value: &T) -> Result<Self, serde_json::Error> {
        let mut w = FnvWriter(FNV_OFFSET);
        serde_json::to_writer(&mut w, value)?;
        Ok(Self(w.0))
    }

    /// The raw hash value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Error parsing a [`StateHash`] from hex.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid state hash {0:?}: expected 16 hex digits")]
pub struct ParseStateHashError(String);

impl FromStr for StateHash {
    type Err = ParseStateHashError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 16 {
            return Err(ParseStateHashError(s.to_owned()));
        }
        u64::from_str_radix(s, 16)
            .map(Self)
            .map_err(|_| ParseStateHashError(s.to_owned()))
    }
}

// Serialized as a hex string: JSON numbers above 2^53 lose precision in many
// languages, and analysis tools should be able to compare hashes as text.
impl Serialize for StateHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StateHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_known_value() {
        // FNV-1a 64 of the empty string is the offset basis; of `null` it is fixed.
        let mut w = FnvWriter(FNV_OFFSET);
        io::Write::write_all(&mut w, b"a").unwrap();
        assert_eq!(w.0, 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn hash_is_stable_and_discriminating() {
        let a = StateHash::of(&[1, 2, 3]).unwrap();
        assert_eq!(a, StateHash::of(&[1, 2, 3]).unwrap());
        assert_ne!(a, StateHash::of(&[1, 2, 4]).unwrap());
    }

    #[test]
    fn hex_round_trip() {
        let h = StateHash::of("hello").unwrap();
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(s.len(), 18);
        let back: StateHash = serde_json::from_str(&s).unwrap();
        assert_eq!(back, h);
        assert!("xyz".parse::<StateHash>().is_err());
        assert!("zzzzzzzzzzzzzzzz".parse::<StateHash>().is_err());
    }
}
