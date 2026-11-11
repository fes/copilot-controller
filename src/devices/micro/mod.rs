//! Offline Micro protocol, input, LED, and snapshot policies.
//!
//! These APIs consume supplied bytes, snapshots, evidence, and clock samples.
//! They never discover or open a device, deliver input, or establish physical
//! qualification. A surrounding driver must enforce the native I/O boundary.
//!
//! Dependencies: `serde` (derive), `serde_json` (with `arbitrary_precision` and
//! `raw_value`), `unicode-general-category` 1.1 (Unicode 16), and `ryu`.
//!
pub mod calibration;
pub mod enumeration;
pub mod error;
pub mod health;
pub mod input;
pub mod led;
pub mod rpc;
pub mod value;
pub mod wire;

mod json;

#[cfg(test)]
mod tests;
