use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};

use super::{ConfigError, ValidationError, Version1, parse_json, present};
use crate::domain::strict::{object, strict_record};
use crate::domain::vocabulary::Action;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brightness(f64);

impl Brightness {
    pub fn new(value: f64) -> Result<Self, ValidationError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ValidationError::Brightness)
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for Brightness {
    fn default() -> Self {
        Self(0.25)
    }
}

impl Serialize for Brightness {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for Brightness {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(f64::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchHost {
    #[default]
    Standalone,
    Integrated,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BindingKey {
    #[serde(rename = "ACT06")]
    Act06,
    #[serde(rename = "ACT07")]
    Act07,
    #[serde(rename = "ACT08")]
    Act08,
    #[serde(rename = "ACT09")]
    Act09,
    #[serde(rename = "ACT12")]
    Act12,
    #[serde(rename = "DPAD_UP")]
    DpadUp,
    #[serde(rename = "DPAD_RIGHT")]
    DpadRight,
    #[serde(rename = "DPAD_DOWN")]
    DpadDown,
    #[serde(rename = "DPAD_LEFT")]
    DpadLeft,
}

impl BindingKey {
    pub const ALL: [Self; 9] = [
        Self::Act06,
        Self::Act07,
        Self::Act08,
        Self::Act09,
        Self::Act12,
        Self::DpadUp,
        Self::DpadRight,
        Self::DpadDown,
        Self::DpadLeft,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Act06 => "ACT06",
            Self::Act07 => "ACT07",
            Self::Act08 => "ACT08",
            Self::Act09 => "ACT09",
            Self::Act12 => "ACT12",
            Self::DpadUp => "DPAD_UP",
            Self::DpadRight => "DPAD_RIGHT",
            Self::DpadDown => "DPAD_DOWN",
            Self::DpadLeft => "DPAD_LEFT",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Act06 => 0,
            Self::Act07 => 1,
            Self::Act08 => 2,
            Self::Act09 => 3,
            Self::Act12 => 4,
            Self::DpadUp => 5,
            Self::DpadRight => 6,
            Self::DpadDown => 7,
            Self::DpadLeft => 8,
        }
    }

    const fn is_dpad(self) -> bool {
        matches!(
            self,
            Self::DpadUp | Self::DpadRight | Self::DpadDown | Self::DpadLeft
        )
    }
}

/// A complete resolved mapping; JSON may supply only the keys being overridden.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlBindings([Action; 9]);

impl Default for ControlBindings {
    fn default() -> Self {
        Self([
            Action::Autopilot,
            Action::Accept,
            Action::Stop,
            Action::Fork,
            Action::Send,
            Action::Up,
            Action::Right,
            Action::Down,
            Action::Left,
        ])
    }
}

impl ControlBindings {
    pub fn get(&self, key: BindingKey) -> Action {
        self.0[key.index()]
    }

    /// Validate before changing the mapping.
    pub fn set(&mut self, key: BindingKey, action: Action) -> Result<(), ValidationError> {
        if key.is_dpad() && action != Action::None && !action.is_direction() {
            return Err(ValidationError::DirectionalBinding);
        }
        self.0[key.index()] = action;
        Ok(())
    }
}

impl Serialize for ControlBindings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(BindingKey::ALL.len()))?;
        for key in BindingKey::ALL {
            map.serialize_entry(key.as_str(), &self.get(key))?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawControlBindings {
    #[serde(rename = "ACT06")]
    act06: Action,
    #[serde(rename = "ACT07")]
    act07: Action,
    #[serde(rename = "ACT08")]
    act08: Action,
    #[serde(rename = "ACT09")]
    act09: Action,
    #[serde(rename = "ACT12")]
    act12: Action,
    #[serde(rename = "DPAD_UP")]
    dpad_up: Action,
    #[serde(rename = "DPAD_RIGHT")]
    dpad_right: Action,
    #[serde(rename = "DPAD_DOWN")]
    dpad_down: Action,
    #[serde(rename = "DPAD_LEFT")]
    dpad_left: Action,
}

impl Default for RawControlBindings {
    fn default() -> Self {
        let defaults = ControlBindings::default();
        Self {
            act06: defaults.get(BindingKey::Act06),
            act07: defaults.get(BindingKey::Act07),
            act08: defaults.get(BindingKey::Act08),
            act09: defaults.get(BindingKey::Act09),
            act12: defaults.get(BindingKey::Act12),
            dpad_up: defaults.get(BindingKey::DpadUp),
            dpad_right: defaults.get(BindingKey::DpadRight),
            dpad_down: defaults.get(BindingKey::DpadDown),
            dpad_left: defaults.get(BindingKey::DpadLeft),
        }
    }
}

impl<'de> Deserialize<'de> for ControlBindings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: RawControlBindings = object(deserializer)?;
        let values = [
            raw.act06,
            raw.act07,
            raw.act08,
            raw.act09,
            raw.act12,
            raw.dpad_up,
            raw.dpad_right,
            raw.dpad_down,
            raw.dpad_left,
        ];
        let mut result = Self::default();
        for (key, action) in BindingKey::ALL.into_iter().zip(values) {
            result.set(key, action).map_err(de::Error::custom)?;
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Right,
    Down,
    Left,
}

impl Direction {
    pub const ALL: [Self; 4] = [Self::Up, Self::Right, Self::Down, Self::Left];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Right => "right",
            Self::Down => "down",
            Self::Left => "left",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Up => 0,
            Self::Right => 1,
            Self::Down => 2,
            Self::Left => 3,
        }
    }
}

/// Optional angles are ordered up/right/down/left for overlap comparisons.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DpadAngles([Option<f64>; 4]);

impl DpadAngles {
    pub fn new(values: [Option<f64>; 4]) -> Result<Self, ValidationError> {
        Self::validate(&values)?;
        Ok(Self(values))
    }

    pub fn get(&self, direction: Direction) -> Option<f64> {
        self.0[direction.index()]
    }

    /// Set or clear one angle. Failure leaves the previous calibration intact.
    pub fn set(&mut self, direction: Direction, angle: Option<f64>) -> Result<(), ValidationError> {
        let mut candidate = self.0;
        candidate[direction.index()] = angle;
        Self::validate(&candidate)?;
        self.0 = candidate;
        Ok(())
    }

    fn validate(values: &[Option<f64>; 4]) -> Result<(), ValidationError> {
        for value in values.iter().flatten() {
            if !value.is_finite() || !(0.0..1.0).contains(value) {
                return Err(ValidationError::CalibrationAngle);
            }
        }

        if let [Some(up), Some(right), Some(down), Some(left)] = *values {
            let angles = [up, right, down, left];
            for first in 0..angles.len() {
                for second in first + 1..angles.len() {
                    let shifted = angles[first] - angles[second] + 0.5;
                    let modulo = shifted - shifted.floor();
                    if (modulo - 0.5).abs() < 0.15 {
                        return Err(ValidationError::CalibrationOverlap);
                    }
                }
            }
        }
        Ok(())
    }
}

impl Serialize for DpadAngles {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.iter().flatten().count()))?;
        for direction in [
            Direction::Down,
            Direction::Left,
            Direction::Right,
            Direction::Up,
        ] {
            if let Some(angle) = self.get(direction) {
                map.serialize_entry(direction.as_str(), &angle)?;
            }
        }
        map.end()
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDpadAngles {
    #[serde(default, deserialize_with = "present")]
    up: Option<f64>,
    #[serde(default, deserialize_with = "present")]
    right: Option<f64>,
    #[serde(default, deserialize_with = "present")]
    down: Option<f64>,
    #[serde(default, deserialize_with = "present")]
    left: Option<f64>,
}

impl<'de> Deserialize<'de> for DpadAngles {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: RawDpadAngles = object(deserializer)?;
        Self::new([raw.up, raw.right, raw.down, raw.left]).map_err(de::Error::custom)
    }
}

strict_record! {
    /// Loading defaults are deliberately separate from the opt-in recommended preset.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct Settings as RawSettings {
        version: Version1,
        #[serde(default)]
        pub brightness: Brightness,
        #[serde(default)]
        pub voice_enabled: bool,
        #[serde(default)]
        pub controls_enabled: bool,
        #[serde(default)]
        pub auto_bind: bool,
        #[serde(default)]
        pub title_probe: bool,
        #[serde(default)]
        pub display_titles: bool,
        #[serde(default)]
        pub window_switcher: bool,
        #[serde(default)]
        pub launch_host: LaunchHost,
        #[serde(default)]
        pub bindings: ControlBindings,
        #[serde(default)]
        pub dpad_angles: DpadAngles,
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: Version1,
            brightness: Brightness::default(),
            voice_enabled: false,
            controls_enabled: false,
            auto_bind: false,
            title_probe: false,
            display_titles: false,
            window_switcher: false,
            launch_host: LaunchHost::default(),
            bindings: ControlBindings::default(),
            dpad_angles: DpadAngles::default(),
        }
    }
}

impl Settings {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        parse_json(bytes)
    }

    pub fn recommended() -> Self {
        Self {
            brightness: Brightness(0.75),
            controls_enabled: true,
            auto_bind: true,
            title_probe: true,
            window_switcher: true,
            ..Self::default()
        }
    }

    pub const fn version(&self) -> u8 {
        1
    }
}
