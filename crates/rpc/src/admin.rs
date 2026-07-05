//! World Chain `admin` namespace extension exposing runtime log-filter control.
//!
//! `admin_tracingDirectives` ephemerally overrides the node's tracing filter
//! (e.g. to raise a module to `trace` while debugging) and automatically
//! reverts to the startup configuration after a caller-supplied TTL, so an
//! elevated filter can never be left on indefinitely.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use jsonrpsee::{
    core::{RpcResult, async_trait},
    proc_macros::rpc,
    types::{ErrorObjectOwned, error::INVALID_PARAMS_CODE},
};
use jsonrpsee_types::error::INTERNAL_ERROR_CODE;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{info, warn};
use world_chain_primitives::tracing::startup_tracing_directives;

/// Maximum permitted TTL for an ephemeral tracing override (1 hour). Bounds how
/// long an elevated filter can degrade the node before auto-reverting.
const MAX_TTL_SECS: u64 = 3600;

/// Maximum length of the directive string accepted from the caller. Bounds
/// untrusted input before it reaches the filter parser.
const MAX_DIRECTIVES_LEN: usize = 1024;

/// Request for [`AdminApiExt::tracing_directives`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracingDirectivesRequest {
    /// `RUST_LOG`-style directive string, e.g. `"info,reth::net=trace"`. The
    /// bare leading token sets the global level; `target=level` entries set
    /// per-target levels.
    pub directives: String,
    /// Seconds until the override is automatically reverted to the node's
    /// startup configuration. Must be in `1..=3600`.
    pub ttl_secs: u64,
}

/// Response for [`AdminApiExt::tracing_directives`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracingDirectivesResponse {
    /// The directive string that was applied.
    pub applied: String,
    /// The TTL, in seconds, after which the override reverts.
    pub ttl_secs: u64,
    /// The directive string the node will revert to when the TTL expires.
    pub reverts_to: String,
}

#[rpc(server, namespace = "admin")]
pub trait AdminApiExt {
    /// Ephemerally override the node's tracing filter directives, reverting to
    /// the startup configuration after `ttlSecs`.
    ///
    /// Requires the node to have been started with the `admin` RPC namespace
    /// enabled (which installs the log-filter reload handle); otherwise returns
    /// an error.
    ///
    /// Scope: reth reloads every reloadable layer together, so the override —
    /// and the subsequent revert — apply *uniformly* to all log outputs (stdout
    /// and the debug log file). The revert target is derived from the stdout
    /// verbosity + `--log.stdout.filter` startup configuration; a layer that was
    /// configured with a *different* startup filter (e.g. a custom
    /// `--log.file.filter`) is restored to this same directive string, not its
    /// own original filter. This is a limitation of reth's uniform reload, not a
    /// separate bug.
    #[method(name = "tracingDirectives")]
    async fn tracing_directives(
        &self,
        req: TracingDirectivesRequest,
    ) -> RpcResult<TracingDirectivesResponse>;
}

/// Bookkeeping for the in-flight ephemeral override, guarded by a single mutex
/// so concurrent `tracingDirectives` calls are serialized.
#[derive(Debug, Default)]
struct RevertState {
    /// Monotonic override counter. Each applied override bumps this, and a
    /// scheduled revert captures the value at schedule time. A revert only fires
    /// if its captured generation still matches — so a timer that has already
    /// woken past its sleep (which `abort` cannot stop) still cannot clobber a
    /// newer override.
    generation: u64,
    /// Handle to the scheduled revert task, aborted when superseded.
    task: Option<JoinHandle<()>>,
}

/// World Chain `admin` namespace extension.
#[derive(Debug, Clone, Default)]
pub struct WorldChainAdminApiExt {
    revert: Arc<Mutex<RevertState>>,
}

impl WorldChainAdminApiExt {
    pub fn new() -> Self {
        Self::default()
    }
}

fn invalid_params(msg: impl ToString) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(INVALID_PARAMS_CODE, msg.to_string(), None::<()>)
}

/// Validate caller input and return the trimmed directive string. Pure (touches
/// no global state) so the bounds are unit-testable without a live subscriber.
fn validate(req: &TracingDirectivesRequest) -> Result<&str, ErrorObjectOwned> {
    if req.ttl_secs == 0 || req.ttl_secs > MAX_TTL_SECS {
        return Err(invalid_params(format!(
            "ttlSecs must be in 1..={MAX_TTL_SECS}"
        )));
    }
    let directives = req.directives.trim();
    if directives.is_empty() {
        return Err(invalid_params("directives must not be empty"));
    }
    if directives.len() > MAX_DIRECTIVES_LEN {
        return Err(invalid_params(format!(
            "directives must be at most {MAX_DIRECTIVES_LEN} characters"
        )));
    }
    Ok(directives)
}

#[async_trait]
impl AdminApiExtServer for WorldChainAdminApiExt {
    async fn tracing_directives(
        &self,
        req: TracingDirectivesRequest,
    ) -> RpcResult<TracingDirectivesResponse> {
        let directives = validate(&req)?;

        if !reth_tracing::log_handle_available() {
            return Err(ErrorObjectOwned::owned(
                INTERNAL_ERROR_CODE,
                "tracing reload is not active; start the node with the `admin` RPC namespace enabled",
                None::<()>,
            ));
        }

        let baseline = startup_tracing_directives().unwrap_or_default().to_string();
        let ttl = req.ttl_secs;

        // Serialize the whole apply-and-schedule under one lock so concurrent
        // calls cannot interleave. The lock is only ever held across synchronous
        // work (never across `.await`), so this cannot deadlock the runtime.
        let mut state = self.revert.lock().expect("revert mutex poisoned");

        // Apply the override. `set_log_vmodule` validates the pattern before
        // mutating any handle, so on failure we return without touching the
        // currently-scheduled revert.
        reth_tracing::set_log_vmodule(directives).map_err(invalid_params)?;

        // Supersede the previous override: abort its revert task (best effort)
        // and bump the generation so a task that already woke is neutered.
        if let Some(previous) = state.task.take() {
            previous.abort();
        }
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;

        let revert = self.revert.clone();
        let revert_baseline = baseline.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(ttl)).await;
            let mut state = revert.lock().expect("revert mutex poisoned");
            // Only revert if this is still the active override; a newer override
            // (or its immediate supersession) leaves the filter as the caller set
            // it.
            if state.generation != generation {
                return;
            }
            match reth_tracing::set_log_vmodule(&revert_baseline) {
                Ok(()) => info!(
                    target: "world_chain::admin",
                    baseline = %revert_baseline,
                    "Reverted tracing directives to startup configuration after TTL"
                ),
                Err(err) => warn!(
                    target: "world_chain::admin",
                    %err,
                    "Failed to revert tracing directives after TTL"
                ),
            }
            state.task = None;
        });
        state.task = Some(task);
        drop(state);

        info!(
            target: "world_chain::admin",
            %directives,
            ttl_secs = ttl,
            "Applied ephemeral tracing directives override"
        );

        Ok(TracingDirectivesResponse {
            applied: directives.to_string(),
            ttl_secs: ttl,
            reverts_to: baseline,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(directives: &str, ttl_secs: u64) -> TracingDirectivesRequest {
        TracingDirectivesRequest {
            directives: directives.to_string(),
            ttl_secs,
        }
    }

    #[test]
    fn rejects_zero_ttl() {
        let err = validate(&req("info", 0)).unwrap_err();
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }

    #[test]
    fn rejects_ttl_over_max() {
        let err = validate(&req("info", MAX_TTL_SECS + 1)).unwrap_err();
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }

    #[test]
    fn rejects_empty_directives() {
        let err = validate(&req("   ", 30)).unwrap_err();
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }

    #[test]
    fn rejects_overlong_directives() {
        let long = "a".repeat(MAX_DIRECTIVES_LEN + 1);
        let err = validate(&req(&long, 30)).unwrap_err();
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }

    #[test]
    fn accepts_valid_input_and_trims() {
        let request = req("  info,reth::net=trace  ", 30);
        let directives = validate(&request).unwrap();
        assert_eq!(directives, "info,reth::net=trace");
    }
}
