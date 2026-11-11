# Bounded native trace projection

`world_chain_rpc::bounded_trace` is an inert result adapter for BLO-1124. It
does not register RPC methods, select a client profile, run an EVM, acquire a
worker permit or establish a native execution deadline. Existing node and
tracer behavior is unchanged.

`project_call_trace` accepts the public pinned native arena, call options,
transaction gas accounting and EVM specification. `project_prestate` accepts
the execution state, prestate options and a read-only provider. Both require
one mutable `ProjectionBudget` and a caller checkpoint returning a typed
cancel/deadline failure. Reuse the same budget across every item in a block;
neither item boundaries nor failures reset its counts.

The limits have explicit units:

| Limit | Unit |
| --- | --- |
| frames_and_logs | Materialized call frames, derived SELFDESTRUCT frames and retained logs combined |
| accounts | Materialized account entries; diff mode counts both pre and post |
| storage_entries | Materialized storage entries, including both pre and post in diff mode |
| data_bytes | Decoded variable payload bytes retained/copied: input, output, revert data, log data/topics and code |
| max_call_depth | EVM parent edges, root depth zero; this is not JSON nesting depth |

These are conservative work/retention counters, not only final-output counts.
Diff entries later removed as unchanged, zero, created or deleted remain charged;
failed frames reserve output plus three times its length for the pinned
decoder's lossy UTF-8 reason. Valid Panic decoding requires at least 36 input
bytes; all known/unknown Panic messages are at most 86 bytes and fit that same
reservation. This proof is specific to pinned alloy-sol-types 1.6.1. No
caller data is truncated into a successful result. A limit failure returns no
partial result and consumed counters are not refunded. An empty arena returns
the native default call frame and charges one frame; an empty prestate charges
no entries. No default limits are supplied by this module.

Checks execute inside frame/log/assembly/account/storage/filtering loops and
before and after synchronous provider operations. Parent indices must point
backward, match native node indices and have consistent EVM depth, preventing
cycles, out-of-order parents and unbounded recursion. `onlyTopCall` visits only
the root, matching the pinned native builder. Construction uses public native
frame helpers with logs disabled, then checks each log before retaining it.
Variable payload limits are checked before adapter-owned large clones.

Provider errors fail closed, including code lookup failures that the pinned
native helper suppresses. Display and Debug are redacted; the original error
remains an internal source and must not be logged as a raw chain. Successful
results preserve the pinned native builder's call/log/revert/gas and ordinary/
diff prestate semantics, including its filtering order.

BLO-1125 still owns EVM/replay cancellation, finite admission, physical worker
and permit lifetime, JSON-encoded depth/size, and cleanup. A synchronous
provider call, frame helper, payload comparison or drop already in progress is
not preempted by these checkpoints. Projection tests cannot qualify the whole
native operation's deadline or a deployed client. No image/config activation
is part of this adapter. It can be reverted normally before a consumer exists.

The modified native conversion material and exact version/license are recorded
in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md). Native parity fixtures
are authored here; upstream test bodies are not copied.
