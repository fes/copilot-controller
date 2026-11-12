use std::{collections::BTreeMap, fmt};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
};

use super::{Binding, ConfigError, Settings, ValidationError, Version1, parse_json};
use crate::domain::identity::{SessionId, Slot};
use crate::domain::strict::object;

const SLOT_KEYS: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Slots([Option<Binding>; 6]);

impl Default for Slots {
    fn default() -> Self {
        Self(std::array::from_fn(|_| None))
    }
}

impl Serialize for Slots {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.iter().flatten().count()))?;
        for (key, binding) in SLOT_KEYS.into_iter().zip(&self.0) {
            if let Some(binding) = binding {
                map.serialize_entry(key, binding)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Slots {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SlotsVisitor;

        impl<'de> Visitor<'de> for SlotsVisitor {
            type Value = Slots;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of slot keys 1 through 6 to bindings")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Slots, M::Error> {
                let mut slots = Slots::default();
                while let Some(key) = map.next_key::<String>()? {
                    let slot = Slot::from_map_key(&key).map_err(de::Error::custom)?;
                    if slots.0[slot.index()].is_some() {
                        return Err(de::Error::custom("duplicate slot key"));
                    }
                    slots.0[slot.index()] = Some(map.next_value()?);
                }
                Ok(slots)
            }
        }

        deserializer.deserialize_map(SlotsVisitor)
    }
}

/// Health supplied by the caller for an existing binding, not a discovery result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingHealth {
    Live,
    ProcessExited,
    PidReused,
    Unresolved,
}

impl BindingHealth {
    fn reusable(self) -> bool {
        matches!(self, Self::ProcessExited | Self::PidReused)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlotMutationError {
    StaleRevision { expected: u64, actual: u64 },
    RevisionExhausted,
    EmptySlot,
    CurrentSessionMismatch,
    DuplicateSession,
    Pinned,
    NotReusable,
    AutoBindDisabled,
    NoCapacity,
    InvalidLabel(ValidationError),
}

impl fmt::Display for SlotMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, actual } => {
                write!(
                    f,
                    "stale slot-map revision: expected {expected}, actual {actual}"
                )
            }
            Self::RevisionExhausted => f.write_str("slot-map revision exhausted"),
            Self::EmptySlot => f.write_str("slot is empty"),
            Self::CurrentSessionMismatch => f.write_str("current session does not match"),
            Self::DuplicateSession => f.write_str("session is already bound to another slot"),
            Self::Pinned => f.write_str("pinned slot cannot be replaced"),
            Self::NotReusable => f.write_str("slot health does not permit replacement"),
            Self::AutoBindDisabled => f.write_str("automatic binding is disabled"),
            Self::NoCapacity => f.write_str("no empty or reusable slot is available"),
            Self::InvalidLabel(error) => write!(f, "invalid custom label: {error}"),
        }
    }
}

impl std::error::Error for SlotMutationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLabel(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoBindOutcome {
    pub slot: Slot,
    pub newly_bound: bool,
    pub revision: u64,
}

/// Six slots with unique sessions and an independently incremented revision.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SlotMap {
    version: Version1,
    revision: u64,
    slots: Slots,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSlotMap {
    version: Version1,
    #[serde(default)]
    revision: u64,
    slots: Slots,
}

impl<'de> Deserialize<'de> for SlotMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: RawSlotMap = object(deserializer)?;
        for (index, binding) in raw.slots.0.iter().enumerate() {
            if let Some(binding) = binding
                && raw.slots.0[index + 1..]
                    .iter()
                    .flatten()
                    .any(|other| other.session_id() == binding.session_id())
            {
                return Err(de::Error::custom("duplicate session ID in slot map"));
            }
        }
        Ok(Self {
            version: raw.version,
            revision: raw.revision,
            slots: raw.slots,
        })
    }
}

impl SlotMap {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        parse_json(bytes)
    }

    pub const fn version(&self) -> u8 {
        1
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, slot: Slot) -> Option<&Binding> {
        self.slots.0[slot.index()].as_ref()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Slot, &Binding)> {
        Slot::ALL
            .into_iter()
            .filter_map(|slot| self.get(slot).map(|binding| (slot, binding)))
    }

    pub fn find_session(&self, session: SessionId) -> Option<Slot> {
        self.iter()
            .find_map(|(slot, binding)| (binding.session_id() == session).then_some(slot))
    }

    fn check_revision(&self, expected: u64) -> Result<(), SlotMutationError> {
        if self.revision != expected {
            return Err(SlotMutationError::StaleRevision {
                expected,
                actual: self.revision,
            });
        }
        Ok(())
    }

    fn next_revision(&self, expected: u64) -> Result<u64, SlotMutationError> {
        self.check_revision(expected)?;
        self.revision
            .checked_add(1)
            .ok_or(SlotMutationError::RevisionExhausted)
    }

    /// Insert or refresh a binding. A supplied expected session must match the
    /// current occupant. Refresh preserves pin and custom label; replacement
    /// requires an unpinned occupant and its exited/reused health. On error
    /// nothing, including the revision, changes.
    pub fn bind(
        &mut self,
        slot: Slot,
        mut binding: Binding,
        expected_revision: u64,
        expected_current: Option<SessionId>,
        current_health: Option<BindingHealth>,
    ) -> Result<u64, SlotMutationError> {
        let next = self.next_revision(expected_revision)?;
        let current = self.get(slot);
        if let Some(expected) = expected_current
            && current.map(Binding::session_id) != Some(expected)
        {
            return Err(SlotMutationError::CurrentSessionMismatch);
        }
        if self
            .find_session(binding.session_id())
            .is_some_and(|other| other != slot)
        {
            return Err(SlotMutationError::DuplicateSession);
        }
        if let Some(current) = current {
            if current.session_id() == binding.session_id() {
                binding.preserve_user_overrides(current);
            } else {
                if current.pinned() {
                    return Err(SlotMutationError::Pinned);
                }
                if !current_health.is_some_and(BindingHealth::reusable) {
                    return Err(SlotMutationError::NotReusable);
                }
            }
        }
        self.slots.0[slot.index()] = Some(binding);
        self.revision = next;
        Ok(next)
    }

    pub fn unbind(
        &mut self,
        slot: Slot,
        expected_session: SessionId,
        expected_revision: u64,
    ) -> Result<Binding, SlotMutationError> {
        let next = self.next_revision(expected_revision)?;
        let binding = self.get(slot).ok_or(SlotMutationError::EmptySlot)?;
        if binding.session_id() != expected_session {
            return Err(SlotMutationError::CurrentSessionMismatch);
        }
        let removed = self.slots.0[slot.index()]
            .take()
            .ok_or(SlotMutationError::EmptySlot)?;
        self.revision = next;
        Ok(removed)
    }

    pub fn set_pinned(
        &mut self,
        slot: Slot,
        pinned: bool,
        expected_revision: u64,
    ) -> Result<u64, SlotMutationError> {
        let next = self.next_revision(expected_revision)?;
        let binding = self.slots.0[slot.index()]
            .as_mut()
            .ok_or(SlotMutationError::EmptySlot)?;
        binding.set_pinned(pinned);
        self.revision = next;
        Ok(next)
    }

    pub fn set_custom_label(
        &mut self,
        slot: Slot,
        label: Option<&str>,
        expected_revision: u64,
    ) -> Result<u64, SlotMutationError> {
        let next = self.next_revision(expected_revision)?;
        let mut binding = self.get(slot).ok_or(SlotMutationError::EmptySlot)?.clone();
        binding
            .set_custom_label(label)
            .map_err(SlotMutationError::InvalidLabel)?;
        self.slots.0[slot.index()] = Some(binding);
        self.revision = next;
        Ok(next)
    }

    /// Bind supplied candidate data; missing health is never treated as reusable.
    ///
    /// `health` is keyed by the current occupant's session identity and must
    /// be supplied by the caller; this method does not inspect a process.
    /// An already bound candidate is a revision-checked no-op, even at
    /// revision exhaustion.
    pub fn auto_bind(
        &mut self,
        settings: &Settings,
        mut candidate: Binding,
        health: &BTreeMap<SessionId, BindingHealth>,
        expected_revision: u64,
    ) -> Result<AutoBindOutcome, SlotMutationError> {
        self.check_revision(expected_revision)?;
        if !settings.auto_bind {
            return Err(SlotMutationError::AutoBindDisabled);
        }
        if let Some(slot) = self.find_session(candidate.session_id()) {
            return Ok(AutoBindOutcome {
                slot,
                newly_bound: false,
                revision: self.revision,
            });
        }
        let next = self.next_revision(expected_revision)?;
        let slot = Slot::ALL
            .into_iter()
            .find(|slot| match self.get(*slot) {
                None => true,
                Some(binding) => {
                    !binding.pinned()
                        && health
                            .get(&binding.session_id())
                            .copied()
                            .is_some_and(BindingHealth::reusable)
                }
            })
            .ok_or(SlotMutationError::NoCapacity)?;
        candidate.prepare_automatic();
        self.slots.0[slot.index()] = Some(candidate);
        self.revision = next;
        Ok(AutoBindOutcome {
            slot,
            newly_bound: true,
            revision: next,
        })
    }
}
