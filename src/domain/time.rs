use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

use super::identity::RuntimeEpoch;
use super::strict::strict_record;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockSource {
    WindowsQpc,
    LinuxMonotonic,
    MacosUptimeRaw,
}

strict_record! {
    /// Ticks in one epoch and frequency. Invalid pairs fail comparison, not open.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    pub struct MonotonicStamp as RawMonotonicStamp {
        pub epoch: RuntimeEpoch,
        pub ticks: u64,
        pub frequency: u64,
    }
}

strict_record! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    pub struct ClockSample as RawClockSample {
        pub source: ClockSource,
        pub stamp: MonotonicStamp,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockError {
    SourceChanged,
    EpochChanged,
    ZeroFrequency,
    FrequencyChanged,
    TicksRegressed,
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SourceChanged => "monotonic clock source changed",
            Self::EpochChanged => "monotonic epoch changed",
            Self::ZeroFrequency => "monotonic clock frequency is zero",
            Self::FrequencyChanged => "monotonic clock frequency changed",
            Self::TicksRegressed => "monotonic ticks regressed",
        })
    }
}

impl std::error::Error for ClockError {}

impl MonotonicStamp {
    fn delta_since(self, earlier: Self) -> Result<u64, ClockError> {
        if self.epoch != earlier.epoch {
            return Err(ClockError::EpochChanged);
        }
        if self.frequency == 0 || earlier.frequency == 0 {
            return Err(ClockError::ZeroFrequency);
        }
        if self.frequency != earlier.frequency {
            return Err(ClockError::FrequencyChanged);
        }
        self.ticks
            .checked_sub(earlier.ticks)
            .ok_or(ClockError::TicksRegressed)
    }

    /// Whole seconds and remainder-derived nanoseconds, truncating fractions.
    pub fn elapsed_since(self, earlier: Self) -> Result<Duration, ClockError> {
        let delta = self.delta_since(earlier)?;
        let seconds = delta / self.frequency;
        let remainder = delta % self.frequency;
        let nanos = u128::from(remainder) * 1_000_000_000 / u128::from(self.frequency);
        Ok(Duration::new(seconds, nanos as u32))
    }

    /// Compare an inclusive deadline exactly, without rounding fractional ticks.
    pub fn elapsed_within(self, earlier: Self, maximum: Duration) -> Result<bool, ClockError> {
        let delta = self.delta_since(earlier)?;
        let elapsed = u128::from(delta) * 1_000_000_000;
        let bound = maximum
            .as_nanos()
            .saturating_mul(u128::from(self.frequency));
        Ok(elapsed <= bound)
    }
}

impl ClockSample {
    pub fn elapsed_since(self, earlier: Self) -> Result<Duration, ClockError> {
        if self.source != earlier.source {
            return Err(ClockError::SourceChanged);
        }
        self.stamp.elapsed_since(earlier.stamp)
    }

    pub fn elapsed_within(self, earlier: Self, maximum: Duration) -> Result<bool, ClockError> {
        if self.source != earlier.source {
            return Err(ClockError::SourceChanged);
        }
        self.stamp.elapsed_within(earlier.stamp, maximum)
    }
}
