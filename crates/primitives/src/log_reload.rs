//! Process-global snapshot of the node's startup log-filter directives.
//!
//! Runtime log-filter reload (see `reth_tracing::set_log_vmodule`) replaces the
//! entire active filter, so it has no notion of "the original configuration".
//! The CLI records the startup directives here when reload is enabled, and the
//! `admin_tracingDirectives` RPC reads them back to revert an ephemeral override
//! once its TTL expires.

use std::sync::OnceLock;

static STARTUP_LOG_DIRECTIVES: OnceLock<String> = OnceLock::new();

/// Records the node's startup log-filter directives. First write wins;
/// subsequent calls are ignored.
pub fn set_startup_log_directives(directives: String) {
    let _ = STARTUP_LOG_DIRECTIVES.set(directives);
}

/// Returns the node's startup log-filter directives, if they were captured.
pub fn startup_log_directives() -> Option<&'static str> {
    STARTUP_LOG_DIRECTIVES.get().map(String::as_str)
}
