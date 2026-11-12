use std::{borrow::Cow, fmt, num::NonZeroU32, num::NonZeroU64};

use serde::{Deserialize, Deserializer, Serialize, de};
use unicode_general_category::{GeneralCategory, get_general_category};

use super::{ConfigError, ValidationError, parse_json, present};
use crate::domain::identity::{SessionId, Slot};
use crate::domain::strict::strict_record;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct Text<const LIMIT: usize>(String);

impl<const LIMIT: usize> Text<LIMIT> {
    fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() || value.chars().count() > LIMIT || value.chars().any(char::is_control)
        {
            return Err(ValidationError::Text);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de, const LIMIT: usize> Deserialize<'de> for Text<LIMIT> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Validate a custom label against Unicode 16.0 general categories.
pub fn validate_custom_label(value: &str) -> Result<(), ValidationError> {
    if value.chars().count() > 80
        || value.chars().any(|character| {
            matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::Surrogate
                    | GeneralCategory::PrivateUse
                    | GeneralCategory::Unassigned
            )
        })
    {
        return Err(ValidationError::CustomLabel);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct CustomLabel(String);

impl CustomLabel {
    fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_custom_label(&value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for CustomLabel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// A lexically checked Windows-shaped compatibility path, not a native state root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct WindowsSessionRoot(String);

fn is_separator(character: char) -> bool {
    character == '\\' || character == '/'
}

fn valid_component(component: &str) -> bool {
    !component.ends_with('.') && !component.ends_with(' ')
}

impl WindowsSessionRoot {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty()
            || value.chars().count() > 4096
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(ValidationError::WindowsPath);
        }

        let bytes = value.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        let unc = bytes.len() >= 2
            && matches!(bytes[0], b'\\' | b'/')
            && matches!(bytes[1], b'\\' | b'/');

        if drive {
            if value[2..].contains(':')
                || value[3..]
                    .split(is_separator)
                    .any(|component| !valid_component(component))
            {
                return Err(ValidationError::WindowsPath);
            }
        } else if unc {
            if value.contains(':') {
                return Err(ValidationError::WindowsPath);
            }
            let mut components = value[2..].split(is_separator);
            let server = components.next().unwrap_or_default();
            let share = components.next().unwrap_or_default();
            if server.is_empty()
                || share.is_empty()
                || !valid_component(server)
                || !valid_component(share)
                || components.any(|component| !valid_component(component))
            {
                return Err(ValidationError::WindowsPath);
            }
        } else {
            return Err(ValidationError::WindowsPath);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WindowsSessionRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// A structurally valid runtime ID; it does not prove a pane or process identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RuntimeId(String);

impl RuntimeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let mut count = 0;
        for component in value.split(',') {
            count += 1;
            let digits = component.strip_prefix('-').unwrap_or(component);
            if count > 32
                || digits.is_empty()
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
                || component.parse::<i32>().is_err()
            {
                return Err(ValidationError::RuntimeId);
            }
        }
        if count < 2 {
            return Err(ValidationError::RuntimeId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RuntimeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

strict_record! {
    /// Legacy terminal anchor data; validation alone does not establish ownership.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct TerminalAnchor as RawTerminalAnchor {
        handle: NonZeroU64,
        host_pid: NonZeroU32,
        host_created: Text<128>,
        runtime_id: RuntimeId,
    }
}

impl TerminalAnchor {
    pub fn new(
        handle: u64,
        host_pid: u32,
        host_created: impl Into<String>,
        runtime_id: RuntimeId,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            handle: NonZeroU64::new(handle).ok_or(ValidationError::PositiveNumber)?,
            host_pid: NonZeroU32::new(host_pid).ok_or(ValidationError::PositiveNumber)?,
            host_created: Text::new(host_created)?,
            runtime_id,
        })
    }

    pub fn handle(&self) -> u64 {
        self.handle.get()
    }

    pub fn host_pid(&self) -> u32 {
        self.host_pid.get()
    }

    pub fn host_created(&self) -> &str {
        self.host_created.as_str()
    }

    pub fn runtime_id(&self) -> &RuntimeId {
        &self.runtime_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assignment {
    Automatic,
    Explicit,
}

strict_record! {
    /// A validated session binding. Mutable access through a slot map is revision-checked.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct Binding as RawBinding {
        session_id: SessionId,
        terminal_title: Text<512>,
        #[serde(default)]
        pinned: bool,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        custom_label: Option<CustomLabel>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        pid: Option<NonZeroU32>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        process_created: Option<Text<128>>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        session_root: Option<WindowsSessionRoot>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        label: Option<Text<512>>,
        #[serde(default)]
        title_prefix: bool,
        #[serde(default)]
        initial_idle_confirmed: bool,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        assignment: Option<Assignment>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        target: Option<TerminalAnchor>,
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        fork_parent: Option<SessionId>,
    }
}

impl Binding {
    pub fn new(
        session_id: SessionId,
        terminal_title: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            session_id,
            terminal_title: Text::new(terminal_title)?,
            pinned: false,
            custom_label: None,
            pid: None,
            process_created: None,
            session_root: None,
            label: None,
            title_prefix: false,
            initial_idle_confirmed: false,
            assignment: None,
            target: None,
            fork_parent: None,
        })
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        parse_json(bytes)
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn terminal_title(&self) -> &str {
        self.terminal_title.as_str()
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    pub fn custom_label(&self) -> Option<&str> {
        self.custom_label.as_ref().map(|value| value.0.as_str())
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid.map(NonZeroU32::get)
    }

    pub fn process_created(&self) -> Option<&str> {
        self.process_created.as_ref().map(Text::as_str)
    }

    pub fn session_root(&self) -> Option<&WindowsSessionRoot> {
        self.session_root.as_ref()
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_ref().map(Text::as_str)
    }

    pub fn title_prefix(&self) -> bool {
        self.title_prefix
    }

    pub fn initial_idle_confirmed(&self) -> bool {
        self.initial_idle_confirmed
    }

    pub fn assignment(&self) -> Option<Assignment> {
        self.assignment
    }

    pub fn target(&self) -> Option<&TerminalAnchor> {
        self.target.as_ref()
    }

    pub fn fork_parent(&self) -> Option<SessionId> {
        self.fork_parent
    }

    pub fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id = session_id;
    }

    pub fn set_terminal_title(&mut self, value: &str) -> Result<(), ValidationError> {
        self.terminal_title = Text::new(value)?;
        Ok(())
    }

    pub fn set_pinned(&mut self, value: bool) {
        self.pinned = value;
    }

    /// Empty and `None` clear the override; parsed explicit empty labels survive.
    pub fn set_custom_label(&mut self, value: Option<&str>) -> Result<(), ValidationError> {
        self.custom_label = match value {
            None | Some("") => None,
            Some(label) => Some(CustomLabel::new(label)?),
        };
        Ok(())
    }

    pub fn set_pid(&mut self, value: Option<u32>) -> Result<(), ValidationError> {
        self.pid = value
            .map(|pid| NonZeroU32::new(pid).ok_or(ValidationError::PositiveNumber))
            .transpose()?;
        Ok(())
    }

    pub fn set_process_created(&mut self, value: Option<&str>) -> Result<(), ValidationError> {
        self.process_created = value.map(Text::new).transpose()?;
        Ok(())
    }

    pub fn set_session_root(&mut self, value: Option<&str>) -> Result<(), ValidationError> {
        self.session_root = value.map(WindowsSessionRoot::new).transpose()?;
        Ok(())
    }

    pub fn set_label(&mut self, value: Option<&str>) -> Result<(), ValidationError> {
        self.label = value.map(Text::new).transpose()?;
        Ok(())
    }

    pub fn set_title_prefix(&mut self, value: bool) {
        self.title_prefix = value;
    }

    pub fn set_initial_idle_confirmed(&mut self, value: bool) {
        self.initial_idle_confirmed = value;
    }

    pub fn set_assignment(&mut self, value: Option<Assignment>) {
        self.assignment = value;
    }

    pub fn set_target(&mut self, value: Option<TerminalAnchor>) {
        self.target = value;
    }

    pub fn set_fork_parent(&mut self, value: Option<SessionId>) {
        self.fork_parent = value;
    }

    /// Title probing does not itself consent to displaying title-derived text.
    pub fn presentation_label(&self, slot: Slot, display_titles: bool) -> Cow<'_, str> {
        if let Some(label) = &self.custom_label {
            Cow::Borrowed(&label.0)
        } else if display_titles {
            Cow::Borrowed(
                self.label
                    .as_ref()
                    .map(Text::as_str)
                    .unwrap_or(self.terminal_title.as_str()),
            )
        } else {
            Cow::Owned(format!("Slot {slot}"))
        }
    }

    pub(super) fn preserve_user_overrides(&mut self, existing: &Self) {
        self.pinned = existing.pinned;
        self.custom_label.clone_from(&existing.custom_label);
    }

    pub(super) fn prepare_automatic(&mut self) {
        self.pinned = false;
        self.custom_label = None;
        self.initial_idle_confirmed = false;
        self.assignment = Some(Assignment::Automatic);
    }
}
