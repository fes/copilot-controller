//! Strict, size-bounded configuration and revision-checked slot mutations.
//!
//! Use `from_json` on a standalone document. Serde deserialization has the
//! same schema validation but relies on an embedding format's own size bound.

mod binding;
mod settings;
mod slots;

pub use binding::{
    Assignment, Binding, RuntimeId, TerminalAnchor, WindowsSessionRoot, validate_custom_label,
};
pub use settings::{
    BindingKey, Brightness, ControlBindings, Direction, DpadAngles, LaunchHost, Settings,
};
pub use slots::{AutoBindOutcome, BindingHealth, SlotMap, SlotMutationError};

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, de::DeserializeOwned};

pub const MAX_CONFIG_BYTES: usize = 65_536;

#[derive(Debug)]
pub enum ConfigError {
    TooLarge { actual: usize, limit: usize },
    InvalidJson(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, limit } => {
                write!(f, "configuration is {actual} bytes; limit is {limit}")
            }
            Self::InvalidJson(error) => write!(f, "invalid configuration: {error}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::TooLarge { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Brightness,
    CalibrationAngle,
    CalibrationOverlap,
    DirectionalBinding,
    Text,
    CustomLabel,
    WindowsPath,
    RuntimeId,
    PositiveNumber,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Brightness => "brightness must be finite and in [0, 1]",
            Self::CalibrationAngle => "D-pad angles must be finite and in [0, 1)",
            Self::CalibrationOverlap => "D-pad directions overlap",
            Self::DirectionalBinding => "a D-pad binding must be a direction or none",
            Self::Text => "text must be nonempty, within its scalar limit, and contain no controls",
            Self::CustomLabel => "custom label contains forbidden characters or is too long",
            Self::WindowsPath => "session root must be a valid absolute Windows path",
            Self::RuntimeId => "runtime ID must contain 2 through 32 comma-separated i32 values",
            Self::PositiveNumber => "identifier must be a positive integer",
        })
    }
}

impl std::error::Error for ValidationError {}

pub(crate) fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            actual: bytes.len(),
            limit: MAX_CONFIG_BYTES,
        });
    }
    serde_json::from_slice(bytes).map_err(ConfigError::InvalidJson)
}

/// Unlike the usual `Option` deserializer, a present JSON null is invalid.
pub(crate) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Version1;

impl Serialize for Version1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}

impl<'de> Deserialize<'de> for Version1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u64::deserialize(deserializer)? {
            1 => Ok(Self),
            _ => Err(de::Error::custom("unsupported configuration version")),
        }
    }
}
