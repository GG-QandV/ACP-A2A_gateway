//! gatewayd/src/lib.rs
//! Library part of the gatewayd binary. Modules live here so that
//! integration tests (gatewayd/tests/) can build Router and
//! Registry without launching the full process: lib does not depend on
//! main() or on reading the config, so the test harness plugs in its own Registry.

pub mod approvals;
pub mod config;
pub mod dialect_probe;
pub mod event_log;
pub mod health;
pub mod journal;
pub mod registry;
pub mod transport_a2a_passthrough;
pub mod transport_http;
pub mod transport_tcp;
