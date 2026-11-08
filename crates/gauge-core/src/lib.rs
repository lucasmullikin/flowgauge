//! Read-only system data for the monitor. Nothing here writes to hardware.

pub mod alerts;
pub mod battery;
pub mod curve;
pub mod dns;
pub mod history;
pub mod ping;
pub mod settings;
pub mod wifi;
pub mod drives;
pub mod geo;
pub mod gpu;
mod iokit;
pub mod memory;
pub mod names;
pub mod network;
pub mod pathcheck;
pub mod processor;
pub mod sensors;
pub mod smart;
pub mod traffic;
