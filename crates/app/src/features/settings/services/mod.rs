//! Blocking Configuration operations, separate from GPUI state and rendering.
//!
//! SettingsView owns task lifecycles and state updates; services execute the
//! existing read-only probes or shell commands on the background executor.

pub(super) mod snapshot;
pub(super) mod updates;
