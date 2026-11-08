//! Identifiers and validated value types used on the wire.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Unique identifier of a game session (UUIDv7, so ids sort by creation time).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct GameId(Uuid);

impl GameId {
    /// A fresh, time-ordered id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for GameId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for GameId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

/// Monotonic per-game decision counter. Every `decide` message carries a new
/// one; actions must echo it, which makes stale or duplicate actions
/// detectable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct DecisionId(u64);

impl DecisionId {
    /// The first decision of a game.
    pub const FIRST: Self = Self(1);

    /// Wraps a raw counter value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw counter value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The following decision id.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Secret that lets a bot reclaim its seat after a disconnect.
///
/// `Debug` is redacted so tokens do not end up in logs.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ResumeToken(String);

impl ResumeToken {
    /// Wraps a token string.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResumeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResumeToken(<redacted>)")
    }
}

/// A developer API key (hosted mode). `Debug` is redacted.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key string.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// A rejected bot name or version.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BotInfoError {
    /// The bot name is empty, too long, padded with whitespace or contains
    /// control characters.
    #[error("bot name must be 1-{max} characters without control characters or surrounding whitespace", max = BotName::MAX_LEN)]
    InvalidName,
    /// The version contains characters outside `[A-Za-z0-9.+_-]` or is too long.
    #[error("bot version must be 1-{max} characters from [A-Za-z0-9.+_-]", max = BotVersion::MAX_LEN)]
    InvalidVersion,
}

/// A self-declared bot name, e.g. `"Strategist"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct BotName(String);

impl BotName {
    /// Maximum length in characters.
    pub const MAX_LEN: usize = 64;

    /// Validates a bot name.
    ///
    /// # Errors
    ///
    /// [`BotInfoError::InvalidName`].
    pub fn new(name: impl Into<String>) -> Result<Self, BotInfoError> {
        let name = name.into();
        let len = name.chars().count();
        let valid = (1..=Self::MAX_LEN).contains(&len)
            && name.trim() == name
            && !name.chars().any(char::is_control);
        valid.then_some(Self(name)).ok_or(BotInfoError::InvalidName)
    }

    /// The name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BotName {
    type Error = BotInfoError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BotName> for String {
    fn from(name: BotName) -> Self {
        name.0
    }
}

impl fmt::Display for BotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A self-declared bot version, e.g. `"2.3.1"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct BotVersion(String);

impl BotVersion {
    /// Maximum length in characters.
    pub const MAX_LEN: usize = 32;

    /// Validates a version string.
    ///
    /// # Errors
    ///
    /// [`BotInfoError::InvalidVersion`].
    pub fn new(version: impl Into<String>) -> Result<Self, BotInfoError> {
        let version = version.into();
        let valid = (1..=Self::MAX_LEN).contains(&version.len())
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'));
        valid
            .then_some(Self(version))
            .ok_or(BotInfoError::InvalidVersion)
    }

    /// The version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BotVersion {
    type Error = BotInfoError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BotVersion> for String {
    fn from(version: BotVersion) -> Self {
        version.0
    }
}

impl fmt::Display for BotVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a bot identifies itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct BotInfo {
    /// Self-declared name.
    pub name: BotName,
    /// Optional self-declared version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<BotVersion>,
}

impl BotInfo {
    /// Validates and builds bot info.
    ///
    /// # Errors
    ///
    /// [`BotInfoError`] if the name or version is invalid.
    pub fn new(name: impl Into<String>, version: Option<&str>) -> Result<Self, BotInfoError> {
        Ok(Self {
            name: BotName::new(name)?,
            version: version.map(BotVersion::new).transpose()?,
        })
    }
}

impl fmt::Display for BotInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{} {v}", self.name),
            None => write!(f, "{}", self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_name_validation() {
        assert!(BotName::new("Strategist").is_ok());
        assert!(BotName::new("ünïcödé ✓").is_ok());
        assert!(BotName::new("").is_err());
        assert!(BotName::new(" padded").is_err());
        assert!(BotName::new("tab\there").is_err());
        assert!(BotName::new("x".repeat(65)).is_err());
        assert!(serde_json::from_str::<BotName>("\"\"").is_err());
    }

    #[test]
    fn bot_version_validation() {
        assert!(BotVersion::new("2.3.1-beta+7").is_ok());
        assert!(BotVersion::new("v 1").is_err());
        assert!(BotVersion::new("").is_err());
        assert!(BotVersion::new("1".repeat(33)).is_err());
    }

    #[test]
    fn bot_info_display_and_json() {
        let info = BotInfo::new("Strat", Some("1.0")).unwrap();
        assert_eq!(info.to_string(), "Strat 1.0");
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(json, r#"{"name":"Strat","version":"1.0"}"#);
        let bare = BotInfo::new("Strat", None).unwrap();
        assert_eq!(serde_json::to_string(&bare).unwrap(), r#"{"name":"Strat"}"#);
    }

    #[test]
    fn secrets_are_redacted() {
        assert_eq!(
            format!("{:?}", ResumeToken::new("abc")),
            "ResumeToken(<redacted>)"
        );
        assert_eq!(format!("{:?}", ApiKey::new("k")), "ApiKey(<redacted>)");
    }

    #[test]
    fn game_id_parses() {
        let id = GameId::new();
        assert_eq!(id.to_string().parse::<GameId>().unwrap(), id);
        assert!("nope".parse::<GameId>().is_err());
        assert!(DecisionId::FIRST < DecisionId::FIRST.next());
    }
}
