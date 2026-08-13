//! Process-global tracing helpers.
//!
//! Holds a snapshot of the node's startup tracing directives.
//!
//! Runtime tracing-filter reload (see `reth_tracing::set_log_vmodule`) replaces
//! the entire active filter, so it has no notion of "the original
//! configuration". The CLI records the startup directives here when reload is
//! enabled, and the `admin_tracingDirectives` RPC reads them back to revert an
//! ephemeral override once its TTL expires.

use std::sync::OnceLock;

static STARTUP_TRACING_DIRECTIVES: OnceLock<String> = OnceLock::new();

/// Records the node's startup tracing directives. First write wins;
/// subsequent calls are ignored.
pub fn set_startup_tracing_directives(directives: String) {
    let _ = STARTUP_TRACING_DIRECTIVES.set(directives);
}

/// Returns the node's startup tracing directives, if they were captured.
pub fn startup_tracing_directives() -> Option<&'static str> {
    STARTUP_TRACING_DIRECTIVES.get().map(String::as_str)
}
