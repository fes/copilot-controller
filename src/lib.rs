//! Pure, offline contracts for the controller foundation.
//!
//! Configuration entry points enforce standalone byte limits. The same types
//! implement strict Serde deserialization for embedding in larger, separately
//! bounded records. Nothing in this library discovers devices or delivers input.

pub mod domain;
