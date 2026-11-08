//! Server configuration and validated per-game settings.

use std::time::Duration;

use ogap_game_core::ForfeitPolicy;
use ogap_protocol::{Limits, ObservationMode};
use thiserror::Error;

/// A think time outside the allowed range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("think time must be between {min} and {max} ms, got {0} ms", min = ThinkTime::MIN_MS, max = ThinkTime::MAX_MS)]
pub struct ThinkTimeError(pub u64);

/// Per-decision time limit, bounded to a sane range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThinkTime(Duration);

impl ThinkTime {
    /// Smallest allowed think time.
    pub const MIN_MS: u64 = 10;
    /// Largest allowed think time (10 minutes).
    pub const MAX_MS: u64 = 600_000;
    /// The platform default: one second.
    pub const DEFAULT: Self = Self(Duration::from_secs(1));

    /// Validates a think time in milliseconds.
    ///
    /// # Errors
    ///
    /// [`ThinkTimeError`] if outside `MIN_MS..=MAX_MS`.
    pub const fn from_millis(ms: u64) -> Result<Self, ThinkTimeError> {
        if ms >= Self::MIN_MS && ms <= Self::MAX_MS {
            Ok(Self(Duration::from_millis(ms)))
        } else {
            Err(ThinkTimeError(ms))
        }
    }

    /// As a [`Duration`].
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }

    /// In whole milliseconds.
    #[must_use]
    pub fn as_millis(self) -> u64 {
        u64::try_from(self.0.as_millis()).unwrap_or(u64::MAX)
    }
}

impl Default for ThinkTime {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Server-wide settings. Defaults suit local development.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Default think time for games that do not specify one.
    pub think_time: ThinkTime,
    /// Default observation mode for games that do not specify one.
    pub observation_mode: ObservationMode,
    /// How long a disconnected bot may stay away before forfeiting (the
    /// decision clock keeps running meanwhile).
    pub reconnect_grace: Duration,
    /// How long a lobby may wait for players before it is cancelled.
    pub lobby_timeout: Duration,
    /// How remaining seats are ranked after a forfeit.
    pub forfeit_policy: ForfeitPolicy,
    /// How long a new connection has to send `hello`.
    pub hello_timeout: Duration,
    /// Largest accepted WebSocket message.
    pub max_message_bytes: u32,
    /// Sustained messages per second per connection (bursts of twice this
    /// are allowed).
    pub max_messages_per_second: u32,
    /// Malformed or rate-limited messages tolerated before disconnecting.
    pub max_violations: u32,
    /// Maximum concurrent connections.
    pub max_connections: usize,
    /// Maximum concurrent game sessions (lobbies included).
    pub max_games: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            think_time: ThinkTime::DEFAULT,
            observation_mode: ObservationMode::default(),
            reconnect_grace: Duration::from_secs(10),
            lobby_timeout: Duration::from_secs(600),
            forfeit_policy: ForfeitPolicy::default(),
            hello_timeout: Duration::from_secs(10),
            max_message_bytes: 64 * 1024,
            max_messages_per_second: 100,
            max_violations: 10,
            max_connections: 10_000,
            max_games: 10_000,
        }
    }
}

impl ServerConfig {
    /// Limits advertised to bots in `welcome`.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        Limits {
            max_message_bytes: self.max_message_bytes,
            max_messages_per_second: self.max_messages_per_second,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_time_bounds() {
        assert!(ThinkTime::from_millis(9).is_err());
        assert!(ThinkTime::from_millis(600_001).is_err());
        assert_eq!(ThinkTime::from_millis(250).unwrap().as_millis(), 250);
        assert_eq!(ThinkTime::default().as_millis(), 1_000);
    }
}
