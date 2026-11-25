//! Steam adapters for Stock Bevy. Without the `steam` feature, no Steam SDK is linked.
//!
//! Applications own Steam initialization, ticket exchange, and save scheduling.
//! The adapters do not require an application state machine or save schema.

pub mod attestation;
pub mod cloud;
pub mod invite_ui;
pub mod watchdog;
