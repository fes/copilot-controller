use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Off,
    Idle,
    Working,
    Complete,
    Waiting,
    Error,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    Autopilot,
    Accept,
    Stop,
    Fork,
    Send,
    ModelPicker,
    Up,
    Right,
    Down,
    Left,
}

impl Action {
    pub const fn is_direction(self) -> bool {
        matches!(self, Self::Up | Self::Right | Self::Down | Self::Left)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    Disabled,
    Unsupported,
    UnknownUi,
    StaleState,
    ClockMismatch,
    WrongTarget,
    ChangedBinding,
    ChangedGeneration,
    ChangedRun,
    ChangedDraft,
    ChangedDialog,
    CommandDraft,
    EmptyDraft,
    Busy,
    BroadApproval,
    Duplicate,
    IndeterminateDelivery,
    PendingInput,
    Unavailable,
    UnsafeContext,
    ExpiredIntent,
}
