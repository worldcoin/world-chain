//! Optional JSON-RPC transport for resumable preconfirmed logs.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_rpc_types::Filter;
use jsonrpsee::{
    PendingSubscriptionSink, SubscriptionSink,
    core::{RpcResult, SubscriptionResult, async_trait},
    proc_macros::rpc,
    types::ErrorObjectOwned,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, watch};
use tracing::warn;

use super::{
    FeedError, FeedStatus, PreconfirmedLogStore, QueryOptions, QueryResult,
    SubscriptionNotification,
};

const MAX_SUBSCRIPTIONS: usize = 64;
const MAX_NOTIFICATION_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_NOTIFICATIONS: usize = 8;
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

struct ActiveSubscription;

impl ActiveSubscription {
    fn new() -> Self {
        metrics::gauge!("worldchain.preconfirmed.active_subscriptions").increment(1.0);
        Self
    }
}

impl Drop for ActiveSubscription {
    fn drop(&mut self) {
        metrics::gauge!("worldchain.preconfirmed.active_subscriptions").decrement(1.0);
    }
}

/// Subscription kinds supported by the versioned World Chain feed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreconfirmedSubscriptionKind {
    PreconfirmedLogs,
}

#[rpc(server, client, namespace = "worldchain")]
pub trait PreconfirmedLogsQueryApi {
    /// Return a current snapshot, or replay events following an opaque cursor.
    #[method(name = "getPreconfirmedLogs")]
    fn get_preconfirmed_logs(
        &self,
        filter: Filter,
        options: Option<QueryOptions>,
    ) -> RpcResult<QueryResult>;
}

#[rpc(server, client, namespace = "worldchain")]
pub trait PreconfirmedLogsSubscriptionApi {
    /// Stream snapshots and resumable updates; terminal errors require client recovery.
    #[subscription(name = "subscribe" => "subscription", unsubscribe = "unsubscribe", item = SubscriptionNotification)]
    async fn subscribe_preconfirmed_logs(
        &self,
        kind: PreconfirmedSubscriptionKind,
        filter: Filter,
        options: Option<QueryOptions>,
    ) -> SubscriptionResult;
}

/// One shared admission limit applies across all connections and cloned RPC modules.
#[derive(Clone, Debug)]
pub struct PreconfirmedLogsRpc {
    store: PreconfirmedLogStore,
    subscriptions: Arc<Semaphore>,
}

impl PreconfirmedLogsRpc {
    pub fn new(store: PreconfirmedLogStore) -> Self {
        Self {
            store,
            subscriptions: Arc::new(Semaphore::new(MAX_SUBSCRIPTIONS)),
        }
    }

    async fn stream(
        &self,
        sink: &SubscriptionSink,
        filter: Filter,
        mut options: QueryOptions,
        mut updates: watch::Receiver<u64>,
        mut result: QueryResult,
    ) -> RpcResult<()> {
        loop {
            options.cursor = Some(result.cursor.clone());
            let has_more = result.has_more;
            let freshness_deadline = tokio::time::Instant::now()
                + self
                    .store
                    .limits()
                    .stale_after
                    .saturating_sub(Duration::from_millis(
                        result.view_age_ms.unwrap_or_default(),
                    ))
                + Duration::from_millis(1);
            send_notification(
                sink,
                SubscriptionNotification::Update {
                    result: Box::new(result),
                },
            )
            .await?;

            if has_more {
                tokio::task::yield_now().await;
            } else {
                tokio::select! {
                    _ = sink.closed() => return Ok(()),
                    _ = tokio::time::sleep_until(freshness_deadline) => {},
                    changed = updates.changed() => {
                        if changed.is_err() {
                            return Err(rpc_error(FeedError::Unavailable {
                                status: FeedStatus::Unavailable,
                                canonical: self.store.checkpoint(),
                            }));
                        }
                    }
                }
            }

            // Mark the wakeup seen before reading, so a concurrent publish triggers another query.
            updates.borrow_and_update();
            result = self
                .store
                .query(filter.clone(), options.clone())
                .map_err(rpc_error)?;
        }
    }
}

impl PreconfirmedLogsQueryApiServer for PreconfirmedLogsRpc {
    fn get_preconfirmed_logs(
        &self,
        filter: Filter,
        options: Option<QueryOptions>,
    ) -> RpcResult<QueryResult> {
        self.store
            .query(filter, options.unwrap_or_default())
            .map_err(rpc_error)
    }
}

#[async_trait]
impl PreconfirmedLogsSubscriptionApiServer for PreconfirmedLogsRpc {
    async fn subscribe_preconfirmed_logs(
        &self,
        pending: PendingSubscriptionSink,
        _kind: PreconfirmedSubscriptionKind,
        filter: Filter,
        options: Option<QueryOptions>,
    ) -> SubscriptionResult {
        let Ok(_permit) = self.subscriptions.clone().try_acquire_owned() else {
            metrics::counter!("worldchain.preconfirmed.subscription_rejections", "reason" => "capacity")
                .increment(1);
            reject(
                pending,
                rpc_error(FeedError::LimitExceeded("too many subscriptions")),
            )
            .await;
            return Ok(());
        };

        let options = options.unwrap_or_default();
        let mut updates = self.store.update_receiver();
        updates.borrow_and_update();
        let initial = match self.store.query(filter.clone(), options.clone()) {
            Ok(result) => result,
            Err(error) => {
                metrics::counter!("worldchain.preconfirmed.subscription_rejections", "reason" => "feed")
                    .increment(1);
                reject(pending, rpc_error(error)).await;
                return Ok(());
            }
        };
        let sink = tokio::time::timeout(SEND_TIMEOUT, pending.accept()).await??;
        let _active = ActiveSubscription::new();

        if let Err(error) = self.stream(&sink, filter, options, updates, initial).await {
            if sink.is_closed() {
                return Ok(());
            }
            metrics::counter!("worldchain.preconfirmed.subscription_errors", "code" => error.code().to_string())
                .increment(1);
            let notification = SubscriptionNotification::Error {
                code: error.code(),
                message: error.message().to_owned(),
                canonical: Some(self.store.checkpoint()),
            };
            if let Err(send_error) = send_notification(&sink, notification).await
                && !sink.is_closed()
            {
                warn!(target: "rpc::preconfirmed", code = send_error.code(),
                    "Closing preconfirmed log subscription without terminal notification");
            }
        }
        Ok(())
    }
}

fn rpc_error(error: FeedError) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(error.code(), error.to_string(), error.checkpoint())
}

async fn reject(pending: PendingSubscriptionSink, error: ErrorObjectOwned) {
    if tokio::time::timeout(SEND_TIMEOUT, pending.reject(error))
        .await
        .is_err()
    {
        warn!(target: "rpc::preconfirmed", "Timed out rejecting preconfirmed log subscription");
    }
}

async fn send_notification(
    sink: &SubscriptionSink,
    notification: SubscriptionNotification,
) -> RpcResult<()> {
    let raw = serde_json::value::to_raw_value(&notification).map_err(|error| {
        ErrorObjectOwned::owned(
            -32603,
            format!("failed to serialize preconfirmed logs: {error}"),
            None::<()>,
        )
    })?;
    if raw.get().len() > MAX_NOTIFICATION_BYTES {
        return Err(rpc_error(FeedError::LimitExceeded(
            "notification is too large",
        )));
    }
    // The underlying queue is shared by this connection's subscriptions.
    let queued = sink.max_capacity().saturating_sub(sink.capacity());
    metrics::histogram!("worldchain.preconfirmed.queued_notifications").record(queued as f64);
    if matches!(notification, SubscriptionNotification::Update { .. })
        && queued >= MAX_QUEUED_NOTIFICATIONS
    {
        return Err(rpc_error(FeedError::LimitExceeded(
            "subscription consumer is too slow",
        )));
    }
    let started = Instant::now();
    let result = sink.send_timeout(raw, SEND_TIMEOUT).await.map_err(|_| {
        rpc_error(FeedError::LimitExceeded(
            "subscription delivery timed out or disconnected",
        ))
    });
    metrics::histogram!("worldchain.preconfirmed.delivery_seconds")
        .record(started.elapsed().as_secs_f64());
    result
}

#[cfg(test)]
mod tests {
    use alloy_rpc_types::Filter;
    use jsonrpsee::{core::server::MethodsError, rpc_params};

    use super::*;
    use crate::preconfirmed::{
        CanonicalCheckpoint, FeedLimits, PreconfirmedView, PreparedLogBlock, ResetReason,
    };
    use alloy_primitives::B256;
    use alloy_rpc_types_engine::PayloadId;

    fn api() -> PreconfirmedLogsRpc {
        PreconfirmedLogsRpc::new(
            PreconfirmedLogStore::new(480, CanonicalCheckpoint::default(), FeedLimits::default())
                .expect("valid feed limits"),
        )
    }

    fn advance(api: &PreconfirmedLogsRpc, index: u64) {
        api.store
            .apply_validated(
                PreparedLogBlock {
                    view: PreconfirmedView {
                        chain_id: 480,
                        parent_hash: B256::ZERO,
                        block_number: 1,
                        block_hash: B256::with_last_byte(index as u8),
                        payload_id: Some(PayloadId::new([1; 8])),
                        flashblock_index: Some(index),
                        timestamp: 1,
                    },
                    logs: Vec::new(),
                },
                Instant::now(),
            )
            .expect("validated test view");
    }

    #[tokio::test]
    async fn unavailable_source_terminates_an_existing_subscription() {
        let api = api();
        advance(&api, 0);
        let module = PreconfirmedLogsSubscriptionApiServer::into_rpc(api.clone());
        let mut subscription = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["preconfirmedLogs", Filter::default()],
                16,
            )
            .await
            .expect("ready subscription");
        let (initial, _) = subscription
            .next::<SubscriptionNotification>()
            .await
            .expect("initial notification")
            .expect("valid notification");
        assert!(matches!(initial, SubscriptionNotification::Update { .. }));
        api.store.mark_unavailable(ResetReason::SourceUnavailable);
        let (terminal, _) = tokio::time::timeout(
            Duration::from_secs(2),
            subscription.next::<SubscriptionNotification>(),
        )
        .await
        .expect("bounded terminal notification")
        .expect("terminal notification")
        .expect("valid notification");
        assert!(matches!(
            terminal,
            SubscriptionNotification::Error { code: -32050, .. }
        ));
        assert_eq!(api.subscriptions.available_permits(), MAX_SUBSCRIPTIONS);
    }

    #[tokio::test]
    async fn stalled_source_becomes_stale_without_a_watch_notification() {
        let store = PreconfirmedLogStore::new(
            480,
            CanonicalCheckpoint::default(),
            FeedLimits {
                stale_after: Duration::from_millis(250),
                ..FeedLimits::default()
            },
        )
        .expect("valid limits");
        let api = PreconfirmedLogsRpc::new(store);
        advance(&api, 0);
        let module = PreconfirmedLogsSubscriptionApiServer::into_rpc(api);
        let mut subscription = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["preconfirmedLogs", Filter::default()],
                16,
            )
            .await
            .expect("ready subscription");
        let (initial, _) = subscription
            .next::<SubscriptionNotification>()
            .await
            .expect("initial notification")
            .expect("valid notification");
        assert!(matches!(initial, SubscriptionNotification::Update { .. }));
        let (terminal, _) = tokio::time::timeout(
            Duration::from_secs(2),
            subscription.next::<SubscriptionNotification>(),
        )
        .await
        .expect("freshness deadline fires without source heartbeat")
        .expect("terminal notification")
        .expect("valid notification");
        assert!(
            matches!(terminal, SubscriptionNotification::Error { code: -32050, message, .. } if message.contains("Stale"))
        );
    }

    #[tokio::test]
    async fn slow_subscriber_is_terminated_without_blocking_the_feed() {
        let api = api();
        advance(&api, 0);
        let cursor = api
            .store
            .query(Filter::default(), QueryOptions::default())
            .expect("initial snapshot")
            .cursor;
        for index in 1..=20 {
            advance(&api, index);
        }
        let module = PreconfirmedLogsSubscriptionApiServer::into_rpc(api.clone());
        let mut subscription = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params![
                    "preconfirmedLogs",
                    Filter::default(),
                    QueryOptions {
                        cursor: Some(cursor),
                        limit: Some(1)
                    }
                ],
                16,
            )
            .await
            .expect("ready subscription");
        tokio::time::timeout(Duration::from_secs(2), async {
            while api.subscriptions.available_permits() != MAX_SUBSCRIPTIONS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("slow subscription releases admission without consuming its queue");
        let mut updates = 0;
        loop {
            let (notification, _) = tokio::time::timeout(
                Duration::from_secs(2),
                subscription.next::<SubscriptionNotification>(),
            )
            .await
            .expect("bounded notification")
            .expect("terminal notification before closure")
            .expect("valid notification");
            match notification {
                SubscriptionNotification::Update { .. } => updates += 1,
                SubscriptionNotification::Error { code, .. } => {
                    assert_eq!(code, -32052);
                    break;
                }
            }
        }
        assert!(updates <= MAX_QUEUED_NOTIFICATIONS);
        advance(&api, 21);
        assert!(
            api.store
                .query(Filter::default(), QueryOptions::default())
                .is_ok()
        );
    }

    #[test]
    fn query_and_subscription_modules_are_separate() {
        let api = api();
        let query = PreconfirmedLogsQueryApiServer::into_rpc(api.clone());
        assert_eq!(
            query.method_names().collect::<Vec<_>>(),
            ["worldchain_getPreconfirmedLogs"]
        );
        let subscription = PreconfirmedLogsSubscriptionApiServer::into_rpc(api);
        let mut names = subscription.method_names().collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["worldchain_subscribe", "worldchain_unsubscribe"]);
    }

    #[tokio::test]
    async fn initializing_feed_rejects_queries_and_subscriptions() {
        let api = api();
        let query = PreconfirmedLogsQueryApiServer::into_rpc(api.clone());
        let error = query
            .call::<_, QueryResult>(
                "worldchain_getPreconfirmedLogs",
                rpc_params![Filter::default()],
            )
            .await
            .expect_err("initializing feed cannot return a snapshot");
        assert!(matches!(error, MethodsError::JsonRpc(error) if error.code() == -32050));

        let subscription = PreconfirmedLogsSubscriptionApiServer::into_rpc(api);
        let error = subscription
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["preconfirmedLogs", Filter::default()],
                8,
            )
            .await
            .expect_err("initializing feed cannot accept subscriptions");
        assert!(matches!(error, MethodsError::JsonRpc(error) if error.code() == -32050));
    }

    #[tokio::test]
    async fn unknown_subscription_kind_is_invalid_params() {
        let module = PreconfirmedLogsSubscriptionApiServer::into_rpc(api());
        let error = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["logs", Filter::default()],
                8,
            )
            .await
            .expect_err("other namespaces' subscription kinds are not supported");
        assert!(matches!(error, MethodsError::JsonRpc(error) if error.code() == -32602));
    }

    #[tokio::test]
    async fn subscription_admission_limit_is_shared_across_modules() {
        let api = api();
        let permit = api
            .subscriptions
            .clone()
            .acquire_many_owned(MAX_SUBSCRIPTIONS as u32)
            .await
            .expect("all subscription permits are initially available");
        let module = PreconfirmedLogsSubscriptionApiServer::into_rpc(api.clone());
        let error = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["preconfirmedLogs", Filter::default()],
                8,
            )
            .await
            .expect_err("shared admission limit applies before acceptance");
        assert!(matches!(error, MethodsError::JsonRpc(error) if error.code() == -32052));
        drop(permit);
        let error = module
            .subscribe(
                "worldchain_subscribe",
                rpc_params!["preconfirmedLogs", Filter::default()],
                8,
            )
            .await
            .expect_err("feed remains initializing after the admission limit is released");
        assert!(matches!(error, MethodsError::JsonRpc(error) if error.code() == -32050));
        assert_eq!(api.subscriptions.available_permits(), MAX_SUBSCRIPTIONS);
    }
}
