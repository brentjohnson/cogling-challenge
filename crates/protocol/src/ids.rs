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

/// Secret that lets a cogling reclaim its seat after a disconnect.
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

/// A rejected cogling name or version.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CoglingInfoError {
    /// The cogling name is empty, too long, padded with whitespace or contains
    /// control characters.
    #[error("cogling name must be 1-{max} characters without control characters or surrounding whitespace", max = CoglingName::MAX_LEN)]
    InvalidName,
    /// The version contains characters outside `[A-Za-z0-9.+_-]` or is too long.
    #[error("cogling version must be 1-{max} characters from [A-Za-z0-9.+_-]", max = CoglingVersion::MAX_LEN)]
    InvalidVersion,
}

/// A self-declared cogling name, e.g. `"Strategist"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct CoglingName(String);

impl CoglingName {
    /// Maximum length in characters.
    pub const MAX_LEN: usize = 64;

    /// Validates a cogling name.
    ///
    /// # Errors
    ///
    /// [`CoglingInfoError::InvalidName`].
    pub fn new(name: impl Into<String>) -> Result<Self, CoglingInfoError> {
        let name = name.into();
        let len = name.chars().count();
        let valid = (1..=Self::MAX_LEN).contains(&len)
            && name.trim() == name
            && !name.chars().any(char::is_control);
        valid
            .then_some(Self(name))
            .ok_or(CoglingInfoError::InvalidName)
    }

    /// The name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CoglingName {
    type Error = CoglingInfoError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CoglingName> for String {
    fn from(name: CoglingName) -> Self {
        name.0
    }
}

impl fmt::Display for CoglingName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A self-declared cogling version, e.g. `"2.3.1"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct CoglingVersion(String);

impl CoglingVersion {
    /// Maximum length in characters.
    pub const MAX_LEN: usize = 32;

    /// Validates a version string.
    ///
    /// # Errors
    ///
    /// [`CoglingInfoError::InvalidVersion`].
    pub fn new(version: impl Into<String>) -> Result<Self, CoglingInfoError> {
        let version = version.into();
        let valid = (1..=Self::MAX_LEN).contains(&version.len())
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'));
        valid
            .then_some(Self(version))
            .ok_or(CoglingInfoError::InvalidVersion)
    }

    /// The version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CoglingVersion {
    type Error = CoglingInfoError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CoglingVersion> for String {
    fn from(version: CoglingVersion) -> Self {
        version.0
    }
}

impl fmt::Display for CoglingVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a cogling identifies itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CoglingInfo {
    /// Self-declared name.
    pub name: CoglingName,
    /// Optional self-declared version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<CoglingVersion>,
}

impl CoglingInfo {
    /// Validates and builds cogling info.
    ///
    /// # Errors
    ///
    /// [`CoglingInfoError`] if the name or version is invalid.
    pub fn new(name: impl Into<String>, version: Option<&str>) -> Result<Self, CoglingInfoError> {
        Ok(Self {
            name: CoglingName::new(name)?,
            version: version.map(CoglingVersion::new).transpose()?,
        })
    }
}

impl fmt::Display for CoglingInfo {
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
    fn cogling_name_validation() {
        assert!(CoglingName::new("Strategist").is_ok());
        assert!(CoglingName::new("ünïcödé ✓").is_ok());
        assert!(CoglingName::new("").is_err());
        assert!(CoglingName::new(" padded").is_err());
        assert!(CoglingName::new("tab\there").is_err());
        assert!(CoglingName::new("x".repeat(65)).is_err());
        assert!(serde_json::from_str::<CoglingName>("\"\"").is_err());
    }

    #[test]
    fn cogling_version_validation() {
        assert!(CoglingVersion::new("2.3.1-beta+7").is_ok());
        assert!(CoglingVersion::new("v 1").is_err());
        assert!(CoglingVersion::new("").is_err());
        assert!(CoglingVersion::new("1".repeat(33)).is_err());
    }

    #[test]
    fn cogling_info_display_and_json() {
        let info = CoglingInfo::new("Strat", Some("1.0")).unwrap();
        assert_eq!(info.to_string(), "Strat 1.0");
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(json, r#"{"name":"Strat","version":"1.0"}"#);
        let bare = CoglingInfo::new("Strat", None).unwrap();
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
