use super::*;
use alloy_primitives::{Address, Bytes, U256};
use alloy_rpc_types::Log;
use alloy_rpc_types_engine::PayloadId;
use std::time::Duration;

fn checkpoint() -> CanonicalCheckpoint {
    CanonicalCheckpoint {
        number: 10,
        hash: B256::repeat_byte(1),
    }
}

fn store(limits: FeedLimits) -> PreconfirmedLogStore {
    PreconfirmedLogStore::new(480, checkpoint(), limits).expect("valid feed limits")
}

fn block(payload: u8, index: u64, ids: &[u8]) -> PreparedLogBlock {
    let hash = keccak256([payload, index as u8]);
    let logs = ids
        .iter()
        .enumerate()
        .map(|(position, id)| PreconfirmedLog {
            event_id: B256::with_last_byte(*id),
            log: Log {
                inner: alloy_primitives::Log::new(
                    Address::repeat_byte(0x42),
                    vec![
                        keccak256("Registered(uint64,uint64,uint256)"),
                        U256::from(7).into(),
                        U256::from(1_800_000_000u64).into(),
                        U256::from(*id).into(),
                    ],
                    Bytes::new(),
                )
                .expect("LOG4 topics"),
                block_hash: Some(hash),
                block_number: Some(11),
                block_timestamp: Some(1_700_000_000),
                transaction_hash: Some(B256::with_last_byte(*id)),
                transaction_index: Some(position as u64),
                log_index: Some(position as u64),
                removed: false,
            },
        })
        .collect();
    PreparedLogBlock {
        view: PreconfirmedView {
            chain_id: 480,
            parent_hash: checkpoint().hash,
            block_number: 11,
            block_hash: hash,
            payload_id: Some(PayloadId::new([payload; 8])),
            flashblock_index: Some(index),
            timestamp: 1_700_000_000,
        },
        logs,
    }
}

fn canonical(mut block: PreparedLogBlock) -> PreparedLogBlock {
    block.view.payload_id = None;
    block.view.flashblock_index = None;
    block
}

fn query(store: &PreconfirmedLogStore) -> QueryResult {
    store
        .query(Filter::new(), QueryOptions::default())
        .expect("ready query")
}

fn poll(store: &PreconfirmedLogStore, cursor: &str, limit: Option<usize>) -> QueryResult {
    store
        .query(
            Filter::new(),
            QueryOptions {
                cursor: Some(cursor.to_owned()),
                limit,
            },
        )
        .expect("resumable query")
}

fn ids(result: &QueryResult) -> Vec<B256> {
    result
        .events
        .iter()
        .flat_map(|event| event.logs.iter().map(|log| log.event_id))
        .collect()
}

#[test]
fn billing_registered_log4_matches_every_indexed_topic() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1, 2]), Instant::now())
        .unwrap();
    let filter = Filter::new()
        .address(Address::repeat_byte(0x42))
        .event_signature(keccak256("Registered(uint64,uint64,uint256)"))
        .topic1(B256::from(U256::from(7)))
        .topic2(B256::from(U256::from(1_800_000_000u64)))
        .topic3(B256::from(U256::from(2)));
    let result = store.query(filter, QueryOptions::default()).unwrap();
    assert_eq!(ids(&result), vec![B256::with_last_byte(2)]);
    assert_eq!(result.events[0].logs[0].log.topics().len(), 4);
    assert_eq!(result.events[0].logs[0].log.log_index, Some(1));
}

#[test]
fn duplicate_and_older_indexes_do_not_refresh_stale_view() {
    let limits = FeedLimits::default();
    let old = Instant::now() - limits.stale_after - Duration::from_secs(1);
    let store = store(limits);
    assert!(matches!(
        store.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Initializing,
            ..
        })
    ));
    store.apply_validated(block(1, 1, &[1]), old).unwrap();
    let sequence = store.inner.lock().sequence;
    let updates = store.update_receiver();
    store
        .apply_validated(block(1, 1, &[1]), Instant::now())
        .unwrap();
    store
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    assert_eq!(store.inner.lock().last_validated, Some(old));
    assert_eq!(store.inner.lock().sequence, sequence);
    assert!(!updates.has_changed().unwrap());
    assert!(matches!(
        store.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Stale,
            ..
        })
    ));
}

#[test]
fn cumulative_views_emit_only_new_logs_and_logless_progress() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    let first = query(&store);
    store
        .apply_validated(block(1, 1, &[1, 2]), Instant::now())
        .unwrap();
    let second = poll(&store, &first.cursor, None);
    assert_eq!(ids(&second), vec![B256::with_last_byte(2)]);
    assert_eq!(second.events[0].kind, LogEventKind::Preconfirmed);
    store
        .apply_validated(block(1, 2, &[1, 2]), Instant::now())
        .unwrap();
    let third = poll(&store, &second.cursor, None);
    assert_eq!(third.events.len(), 1);
    assert_eq!(third.events[0].kind, LogEventKind::Progress);
    assert!(third.events[0].logs.is_empty());
    assert_eq!(third.view.unwrap().flashblock_index, Some(2));
}

#[test]
fn conflicting_index_and_source_gaps_reset_and_wake_consumers() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    let first = query(&store);
    let updates = store.update_receiver();
    let mut conflicting = block(1, 0, &[1]);
    conflicting.view.block_hash = B256::repeat_byte(9);
    assert!(matches!(
        store.apply_validated(conflicting, Instant::now()),
        Err(FeedError::ResyncRequired { .. })
    ));
    assert!(updates.has_changed().unwrap());
    assert!(matches!(
        store.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Unavailable,
            ..
        })
    ));
    assert!(store.inner.lock().current.is_none());
    store
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    let reset = poll(&store, &first.cursor, None);
    assert_eq!(reset.events[0].reason, Some(ResetReason::InvalidSource));
    let before_gap = query(&store);
    store
        .apply_validated(block(1, 2, &[1, 2]), Instant::now())
        .unwrap();
    let gap = poll(&store, &before_gap.cursor, None);
    assert_eq!(gap.events[0].reason, Some(ResetReason::SourceGap));
    assert_eq!(
        ids(&gap),
        vec![B256::with_last_byte(1), B256::with_last_byte(2)]
    );
}

#[test]
fn replacement_promotion_and_reorg_have_explicit_log_lifecycles() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1, 2]), Instant::now())
        .unwrap();
    let first = query(&store);
    store
        .apply_validated(block(2, 0, &[3]), Instant::now())
        .unwrap();
    let replacement = poll(&store, &first.cursor, None);
    assert_eq!(replacement.events[0].kind, LogEventKind::Removed);
    assert!(replacement.events[0].logs.iter().all(|log| log.log.removed));
    assert_eq!(replacement.events[1].kind, LogEventKind::Preconfirmed);

    let committed = canonical(block(2, 1, &[3, 4]));
    let next = CanonicalCheckpoint {
        number: 11,
        hash: committed.view.block_hash,
    };
    store
        .apply_canonical(vec![], vec![committed.clone()], next)
        .unwrap();
    let promotion = poll(&store, &replacement.cursor, None);
    assert_eq!(promotion.events[0].kind, LogEventKind::Canonical);
    assert!(
        promotion
            .events
            .iter()
            .all(|event| event.kind != LogEventKind::Removed)
    );
    assert_eq!(
        ids(&promotion),
        vec![B256::with_last_byte(3), B256::with_last_byte(4)]
    );
    assert!(promotion.view.is_none());
    assert_eq!(promotion.canonical, next);

    let alternative = canonical(block(3, 0, &[5]));
    let next = CanonicalCheckpoint {
        number: 11,
        hash: alternative.view.block_hash,
    };
    store
        .apply_canonical(vec![committed], vec![alternative], next)
        .unwrap();
    let reorg = poll(&store, &promotion.cursor, None);
    assert_eq!(reorg.events[0].reason, Some(ResetReason::CanonicalReorg));
    assert!(reorg.events[0].logs.iter().all(|log| log.log.removed));
    assert_eq!(reorg.events[1].kind, LogEventKind::Canonical);
    assert_eq!(reorg.canonical, next);
}

#[test]
fn canonical_promotion_removes_only_unaccepted_preconfirmed_logs() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1, 2]), Instant::now())
        .unwrap();
    let first = query(&store);
    let committed = canonical(block(2, 0, &[1, 3]));
    let next = CanonicalCheckpoint {
        number: 11,
        hash: committed.view.block_hash,
    };
    store
        .apply_canonical(vec![], vec![committed], next)
        .unwrap();
    let result = poll(&store, &first.cursor, None);
    assert_eq!(result.events[0].kind, LogEventKind::Removed);
    assert_eq!(result.events[0].logs.len(), 1);
    assert_eq!(result.events[0].logs[0].event_id, B256::with_last_byte(2));
    assert_eq!(result.events[1].kind, LogEventKind::Canonical);
}

#[test]
fn snapshot_pages_are_immutable_then_replay_live_updates_without_gaps() {
    let store = store(FeedLimits {
        max_page_logs: 2,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[1, 2, 3, 4, 5]), Instant::now())
        .unwrap();
    let first = query(&store);
    assert!(first.has_more);
    assert_eq!(
        ids(&first),
        vec![B256::with_last_byte(1), B256::with_last_byte(2)]
    );
    store
        .apply_validated(block(1, 1, &[1, 2, 3, 4, 5, 6]), Instant::now())
        .unwrap();
    let second = poll(&store, &first.cursor, None);
    let third = poll(&store, &second.cursor, None);
    assert_eq!(second.view, first.view);
    assert_eq!(third.view, first.view);
    assert_eq!(
        ids(&second),
        vec![B256::with_last_byte(3), B256::with_last_byte(4)]
    );
    assert_eq!(ids(&third), vec![B256::with_last_byte(5)]);
    assert!(third.has_more);
    let live = poll(&store, &third.cursor, None);
    assert_eq!(ids(&live), vec![B256::with_last_byte(6)]);
    assert!(!live.has_more);
}

#[test]
fn complete_snapshots_are_not_cached_and_pagination_eviction_is_explicit() {
    let store = store(FeedLimits {
        max_page_logs: 2,
        max_snapshot_pages: 1,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    for _ in 0..10 {
        assert!(!query(&store).has_more);
    }
    assert!(store.inner.lock().snapshots.is_empty());
    store
        .apply_validated(block(1, 1, &[1, 2, 3]), Instant::now())
        .unwrap();
    let first = query(&store);
    let second = query(&store);
    assert!(second.has_more);
    assert_eq!(store.inner.lock().snapshots.len(), 1);
    assert!(matches!(
        store.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(first.cursor),
                limit: None
            }
        ),
        Err(FeedError::ResyncRequired { .. })
    ));
}

#[test]
fn cursor_scope_offsets_and_schema_are_validated() {
    let store = store(FeedLimits {
        max_page_logs: 1,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[1, 2, 3]), Instant::now())
        .unwrap();
    let first = query(&store);
    let options = QueryOptions {
        cursor: Some(first.cursor.clone()),
        limit: None,
    };
    assert!(matches!(
        store.query(Filter::new().address(Address::ZERO), options.clone()),
        Err(FeedError::InvalidCursor(_))
    ));
    let restarted = self::store(FeedLimits::default());
    restarted
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    assert!(matches!(
        restarted.query(Filter::new(), options),
        Err(FeedError::ResyncRequired { .. })
    ));

    for offset in [3, usize::MAX] {
        let mut cursor = Cursor::decode(&first.cursor).unwrap();
        cursor.offset = offset;
        assert!(matches!(
            store.query(
                Filter::new(),
                QueryOptions {
                    cursor: Some(cursor.encode().unwrap()),
                    limit: None
                }
            ),
            Err(FeedError::InvalidCursor(_))
        ));
    }
    for encoded in [
        "invalid".to_owned(),
        "0".repeat(MAX_CURSOR_BYTES + 1),
        hex::encode(br#"{"version":1,"unknown":true}"#),
    ] {
        assert!(matches!(
            store.query(
                Filter::new(),
                QueryOptions {
                    cursor: Some(encoded),
                    limit: None
                }
            ),
            Err(FeedError::InvalidCursor(_))
        ));
    }
    assert!(matches!(
        store.query(Filter::new().from_block(1), QueryOptions::default()),
        Err(FeedError::InvalidFilter(_))
    ));
}

#[test]
fn normalized_or_filters_can_resume_the_same_snapshot() {
    let store = store(FeedLimits {
        max_page_logs: 1,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[1, 2]), Instant::now())
        .unwrap();
    let address = Address::repeat_byte(0x42);
    let signature = keccak256("Registered(uint64,uint64,uint256)");
    let a = Filter::new()
        .address(vec![address, Address::ZERO, address])
        .event_signature(vec![signature, B256::ZERO, signature]);
    let b = Filter::new()
        .address(vec![Address::ZERO, address])
        .event_signature(vec![B256::ZERO, signature]);
    let first = store.query(a, QueryOptions::default()).unwrap();
    let second = store
        .query(
            b,
            QueryOptions {
                cursor: Some(first.cursor),
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(ids(&second), vec![B256::with_last_byte(2)]);
}

#[test]
fn event_pagination_validates_partial_offsets_and_makes_progress() {
    let store = store(FeedLimits {
        max_page_logs: 1,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    let initial = query(&store);
    store
        .apply_validated(block(1, 1, &[1, 2, 3]), Instant::now())
        .unwrap();
    let first = poll(&store, &initial.cursor, None);
    let mut invalid = Cursor::decode(&first.cursor).unwrap();
    invalid.offset = 3;
    assert!(matches!(
        store.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(invalid.encode().unwrap()),
                limit: None
            }
        ),
        Err(FeedError::InvalidCursor(_))
    ));
    let second = poll(&store, &first.cursor, None);
    let third = poll(&store, &second.cursor, None);
    assert_eq!(ids(&first), vec![B256::with_last_byte(1)]);
    assert_eq!(ids(&second), vec![B256::with_last_byte(2)]);
    assert_eq!(ids(&third), vec![B256::with_last_byte(3)]);
    assert!(!third.has_more);
    let mut invalid = Cursor::decode(&third.cursor).unwrap();
    invalid.offset = 1;
    assert!(matches!(
        store.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(invalid.encode().unwrap()),
                limit: None
            }
        ),
        Err(FeedError::InvalidCursor(_))
    ));
}

#[test]
fn later_event_that_fits_a_fresh_byte_page_does_not_fail_current_page() {
    let store = store(FeedLimits {
        max_page_bytes: 5_000,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    let initial = query(&store);
    let mut first = block(1, 1, &[1]);
    first.logs[0].log.inner.data.data = Bytes::from(vec![0xaa; 700]);
    store
        .apply_validated(first.clone(), Instant::now())
        .unwrap();
    let mut second = block(1, 2, &[1, 2]);
    second.logs[0].log.inner.data.data = first.logs[0].log.inner.data.data.clone();
    second.logs[1].log.inner.data.data = Bytes::from(vec![0xbb; 700]);
    store.apply_validated(second, Instant::now()).unwrap();
    let first = poll(&store, &initial.cursor, None);
    assert_eq!(ids(&first), vec![B256::with_last_byte(1)]);
    assert!(first.has_more);
    assert!(serialized_size(&first).unwrap() <= store.limits.max_page_bytes);
    let second = poll(&store, &first.cursor, None);
    assert_eq!(ids(&second), vec![B256::with_last_byte(2)]);
    assert!(!second.has_more);
    assert!(serialized_size(&second).unwrap() <= store.limits.max_page_bytes);
}

#[test]
fn event_retention_and_append_failure_are_bounded_and_recoverable() {
    let store = store(FeedLimits {
        max_history_events: 2,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    let initial = query(&store);
    for index in 1..=3 {
        store
            .apply_validated(block(1, index, &[]), Instant::now())
            .unwrap();
    }
    assert_eq!(store.inner.lock().history.len(), 2);
    assert!(matches!(
        store.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(initial.cursor),
                limit: None
            }
        ),
        Err(FeedError::ResyncRequired { .. })
    ));

    let limited = self::store(FeedLimits {
        max_history_bytes: MIN_RETAINED_BYTES,
        ..Default::default()
    });
    limited
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    let before = query(&limited);
    let updates = limited.update_receiver();
    let mut large = block(1, 1, &[1]);
    large.logs[0].log.inner.data.data = Bytes::from(vec![0xff; 2_000]);
    assert!(matches!(
        limited.apply_validated(large, Instant::now()),
        Err(FeedError::LimitExceeded(_))
    ));
    assert!(updates.has_changed().unwrap());
    assert!(limited.inner.lock().history.is_empty());
    assert!(limited.inner.lock().current.is_none());
    assert!(limited.inner.lock().unavailable);
    assert!(matches!(
        limited.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(before.cursor),
                limit: None
            }
        ),
        Err(FeedError::ResyncRequired { .. })
    ));
    assert!(matches!(
        limited.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Unavailable,
            ..
        })
    ));
    limited
        .apply_validated(block(1, 2, &[]), Instant::now())
        .unwrap();
    assert_eq!(query(&limited).status, FeedStatus::Ready);
}

#[test]
fn oversized_views_logs_and_snapshot_cache_have_explicit_limits() {
    let store = store(FeedLimits {
        max_view_logs: 1,
        ..Default::default()
    });
    assert!(matches!(
        store.apply_validated(block(1, 0, &[1, 2]), Instant::now()),
        Err(FeedError::LimitExceeded(_))
    ));
    assert!(store.inner.lock().unavailable);

    let bytes = self::store(FeedLimits {
        max_page_bytes: 2_500,
        ..Default::default()
    });
    let mut large = block(1, 0, &[1]);
    large.logs[0].log.inner.data.data = Bytes::from(vec![0xaa; 2_000]);
    bytes.apply_validated(large, Instant::now()).unwrap();
    assert!(matches!(
        bytes.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::LimitExceeded(_))
    ));
    assert!(bytes.inner.lock().snapshots.is_empty());

    let cached = self::store(FeedLimits {
        max_view_bytes: 8_192,
        max_snapshot_bytes: 8_192,
        max_page_logs: 1,
        ..Default::default()
    });
    cached
        .apply_validated(block(1, 0, &[1, 2, 3, 4, 5]), Instant::now())
        .unwrap();
    let oldest = query(&cached);
    for _ in 0..4 {
        assert!(query(&cached).has_more);
    }
    let state = cached.inner.lock();
    assert!(state.snapshot_bytes <= cached.limits.max_snapshot_bytes);
    assert!(state.snapshots.len() <= cached.limits.max_snapshot_pages);
    drop(state);
    assert!(matches!(
        cached.query(
            Filter::new(),
            QueryOptions {
                cursor: Some(oldest.cursor),
                limit: None
            }
        ),
        Err(FeedError::ResyncRequired { .. })
    ));
}

#[test]
fn source_reset_is_replayable_and_unavailability_requires_a_new_view() {
    let store = store(FeedLimits::default());
    store
        .apply_validated(block(1, 0, &[1]), Instant::now())
        .unwrap();
    let before = query(&store);
    store.reset_to(checkpoint(), ResetReason::SourceGap);
    assert!(matches!(
        store.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Unavailable,
            ..
        })
    ));
    store
        .apply_validated(block(1, 1, &[1, 2]), Instant::now())
        .unwrap();
    let reset = poll(&store, &before.cursor, None);
    assert_eq!(reset.events[0].kind, LogEventKind::Reset);
    assert_eq!(reset.events[0].reason, Some(ResetReason::SourceGap));
    assert_eq!(reset.view.as_ref().unwrap().flashblock_index, Some(1));
    store.mark_unavailable(ResetReason::SourceUnavailable);
    assert!(matches!(
        store.query(Filter::new(), QueryOptions::default()),
        Err(FeedError::Unavailable {
            status: FeedStatus::Unavailable,
            ..
        })
    ));
    store
        .apply_validated(block(1, 2, &[1, 2]), Instant::now())
        .unwrap();
    assert_eq!(query(&store).status, FeedStatus::Ready);
}

#[test]
fn logless_control_events_obey_page_count_and_invalid_limits_are_rejected() {
    let store = store(FeedLimits {
        max_page_logs: 1,
        max_filter_items: 1,
        ..Default::default()
    });
    store
        .apply_validated(block(1, 0, &[]), Instant::now())
        .unwrap();
    let first = query(&store);
    for index in 1..=3 {
        store
            .apply_validated(block(1, index, &[]), Instant::now())
            .unwrap();
    }
    let mut cursor = first.cursor;
    for index in 1..=3 {
        let page = poll(&store, &cursor, None);
        assert_eq!(page.events.len(), 1);
        assert!(page.events[0].logs.is_empty());
        assert_eq!(page.has_more, index != 3);
        cursor = page.cursor;
    }
    for limit in [0, 2] {
        assert!(matches!(
            store.query(
                Filter::new(),
                QueryOptions {
                    cursor: None,
                    limit: Some(limit)
                }
            ),
            Err(FeedError::LimitExceeded(_))
        ));
    }
    assert!(matches!(
        store.query(
            Filter::new().address(vec![Address::ZERO, Address::repeat_byte(1)]),
            QueryOptions::default()
        ),
        Err(FeedError::InvalidFilter(_))
    ));
    assert!(matches!(
        PreconfirmedLogStore::new(
            480,
            checkpoint(),
            FeedLimits {
                max_history_events: 0,
                ..Default::default()
            }
        ),
        Err(FeedError::LimitExceeded(_))
    ));
}
