//! Offline firmware planning and an injectable maintenance transaction model.
//!
//! This module contains no native device, process, registry, lease, or supervisor
//! binding. In particular, a successful transaction against a fake backend is
//! not device qualification or authorization to perform maintenance.
//!
//! The worker entry point fails closed. Do not expose the injected model as a
//! CLI worker: native integration must independently provide the supervised
//! process group, cancellation and output bounds, shared lease, qualified USB
//! and registry bindings, and race-resistant approved APFS recovery roots.
//! Ancestor, ownership, ACL, and substitution checks are not supplied by this
//! synthetic model's workspace helper.
//!
//! Tests require `COPILOT_FIRMWARE_FIXTURE_ROOT` to explicitly name an existing
//! fixture root; each test creates and later removes only its own private child.

#![forbid(unsafe_code)]

mod artifacts;
mod backend;
mod identity;
mod options;
mod plan;
mod report;
mod transaction;
mod workspace;

pub use backend::{
    BackendError, HidCandidate, HidOpened, HidTransport, MaintenanceBackend, RegistryNode,
    RegistryQuery, RegistrySnapshot, ToolInvocation, ToolOperation, ToolOutput, WorkerContext,
    WorkerPlatform,
};
pub use options::{CONFIRMATION, OptionError, UpdateOptions, validate_options};
pub use plan::{
    FirmwareCapability, FirmwarePlan, MaintenanceEvidence, MaintenancePath, MaintenancePlatform,
    MaintenanceTransport, micro_capability, offline_plan, unavailable_capability,
};
pub use report::{UpdateOutcome, UpdatePhase, UpdateReport};
pub use transaction::{run_maintenance_worker, run_transaction_model};

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareError {
    message: String,
}

impl FirmwareError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FirmwareError {}

#[cfg(test)]
mod tests;
