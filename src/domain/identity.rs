use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

use super::strict::{object, strict_record};

/// One of the six physical/logical slots. JSON representation is an integer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Slot(u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotError;

impl fmt::Display for SlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("slot must be an integer from 1 through 6")
    }
}

impl std::error::Error for SlotError {}

impl Slot {
    pub const ALL: [Self; 6] = [Self(1), Self(2), Self(3), Self(4), Self(5), Self(6)];

    pub const fn new(value: u8) -> Result<Self, SlotError> {
        if value >= 1 && value <= 6 {
            Ok(Self(value))
        } else {
            Err(SlotError)
        }
    }

    pub const fn number(self) -> u8 {
        self.0
    }

    pub(crate) const fn index(self) -> usize {
        (self.0 - 1) as usize
    }

    pub(crate) fn from_map_key(key: &str) -> Result<Self, SlotError> {
        match key {
            "1" => Ok(Self(1)),
            "2" => Ok(Self(2)),
            "3" => Ok(Self(3)),
            "4" => Ok(Self(4)),
            "5" => Ok(Self(5)),
            "6" => Ok(Self(6)),
            _ => Err(SlotError),
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<u8> for Slot {
    type Error = SlotError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<u64> for Slot {
    type Error = SlotError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        u8::try_from(value)
            .map_err(|_| SlotError)
            .and_then(Self::new)
    }
}

impl TryFrom<i64> for Slot {
    type Error = SlotError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        u8::try_from(value)
            .map_err(|_| SlotError)
            .and_then(Self::new)
    }
}

impl Serialize for Slot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Slot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(u64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UuidTextError;

impl fmt::Display for UuidTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UUID must use 36-character hyphenated hexadecimal text")
    }
}

impl std::error::Error for UuidTextError {}

fn parse_canonical_uuid(text: &str) -> Result<Uuid, UuidTextError> {
    let bytes = text.as_bytes();
    if bytes.len() != 36
        || bytes.iter().enumerate().any(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte != b'-'
            } else {
                !byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(UuidTextError);
    }
    Uuid::parse_str(text).map_err(|_| UuidTextError)
}

macro_rules! uuid_identity {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            /// Wrap an existing UUID without imposing a nonnil requirement.
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Parse hyphenated canonical text, accepting either hex case.
            pub fn parse_str(text: &str) -> Result<Self, UuidTextError> {
                parse_canonical_uuid(text).map(Self)
            }

            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            pub const fn is_nil(&self) -> bool {
                self.0.is_nil()
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self::from_uuid(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = UuidTextError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::parse_str(text)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.hyphenated().fmt(f)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0.hyphenated())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::parse_str(&text).map_err(de::Error::custom)
            }
        }
    };
}

uuid_identity!(
    /// UUID identifying a session, independent of any process or target.
    SessionId
);
uuid_identity!(
    /// UUID identifying one runtime/lifecycle epoch.
    RuntimeEpoch
);

impl RuntimeEpoch {
    /// Create a fresh random epoch for a new runtime or lifecycle cut.
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

strict_record! {
    /// Windows-specific process lifetime identity. Zero values remain representable;
    /// operations requiring positive lifetime evidence must check that separately.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
    pub struct NativeProcessIdentity as RawNativeProcessIdentity {
        pub pid: u32,
        pub creation_time_100ns: u64,
        pub session_id: u32,
    }
}

/// A host-supplied opaque identifier, limited by UTF-8 bytes rather than scalars.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OpaqueId(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpaqueIdError {
    Empty,
    TooLong,
    ControlCharacter,
}

impl fmt::Display for OpaqueIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "opaque ID must not be empty",
            Self::TooLong => "opaque ID exceeds 256 UTF-8 bytes",
            Self::ControlCharacter => "opaque ID contains a control character",
        })
    }
}

impl std::error::Error for OpaqueIdError {}

impl OpaqueId {
    pub fn new(value: impl Into<String>) -> Result<Self, OpaqueIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(OpaqueIdError::Empty);
        }
        if value.len() > 256 {
            return Err(OpaqueIdError::TooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(OpaqueIdError::ControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for OpaqueId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OpaqueId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Backend identities are distinct alternatives, not conversions of a handle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetIdentity {
    WindowsNative {
        process: NativeProcessIdentity,
        window: u64,
    },
    IntegratedHost {
        host_epoch: RuntimeEpoch,
        target: OpaqueId,
        attachment: OpaqueId,
    },
}

#[derive(Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
enum RawTargetIdentity {
    WindowsNative {
        process: NativeProcessIdentity,
        window: u64,
    },
    IntegratedHost {
        host_epoch: RuntimeEpoch,
        target: OpaqueId,
        attachment: OpaqueId,
    },
}

impl<'de> Deserialize<'de> for TargetIdentity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let target: RawTargetIdentity = object(deserializer)?;
        Ok(match target {
            RawTargetIdentity::WindowsNative { process, window } => {
                Self::WindowsNative { process, window }
            }
            RawTargetIdentity::IntegratedHost {
                host_epoch,
                target,
                attachment,
            } => Self::IntegratedHost {
                host_epoch,
                target,
                attachment,
            },
        })
    }
}

strict_record! {
    /// Independently tracked generations; equality compares all five counters.
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize)]
    pub struct Generations as RawGenerations {
        pub connection: u64,
        pub profile: u64,
        pub selection: u64,
        pub binding: u64,
        pub presentation: u64,
    }
}
