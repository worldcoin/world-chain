# Flashblock signing evidence

For op-node unsafe-block signatures, see [unsafe-block evidence](unsafe-block-evidence.md).

Add `--rpc.flashblock-evidence` to a node running `--flashblocks.enabled`.
The feature defaults off. Include `eth` in the HTTP/WebSocket namespace selection
for each transport that should expose the lookup. No additional private keys are
required. `op_supportedCapabilities` then includes
`worldchain_flashblock_evidence_v1` on those transports.

## Lookup

`worldchain_getFlashblockEvidence` accepts one immutable selector:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "worldchain_getFlashblockEvidence",
  "params": [{"payloadId": "0x0000000000000000", "index": 0,
    "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000000"}]
}
```

Use the actual payload ID, Flashblock index and block hash from the desired view
(for example, the `view` of a preconfirmed-log response). The result contains
`version: 1`, a hexadecimal `chainId`, the requested `id`, and `wire`: the original
hexadecimal, uncompressed `flblk` frame including its message-type byte. Incoming
frames retain the exact bytes verified at ingress; local frames retain the bytes
used for publication. Captures become queryable after ordered P2P delivery.

This is signature evidence, not a claim that execution succeeded or that the view
became unsafe, canonical, safe or finalized. Evidence already captured for a
replaced view can remain available for its original immutable selector.

## Independent verification

Use `world_chain_primitives::evidence::FlashblockEvidence::verify`, supplying a
separately trusted network-specific authorizer key, expected chain and expected
selector. A reusable command-line example accepts a saved JSON **result object**:

```sh
cargo run --locked -p world-chain-primitives --example verify-flashblock-evidence -- \
  evidence.json "$TRUSTED_AUTHORIZER_HEX" 480 "$PAYLOAD_ID" "$INDEX" "$BLOCK_HASH"
```

The authorizer's Ed25519 signature authenticates the delegation over
`blake3(payload_id || timestamp_le64 || builder_public_key)`. The builder signs
`blake3(rlp(message) || rlp(authorization))`. Metadata contains JSON **inside** that
RLP preimage: decoding and re-encoding it can invalidate a correct signature.
The verifier checks signatures before decoding the payload body.

`crates/primitives/testdata/flashblock-evidence-v1.json` contains a synthetic
public verification vector with intentionally non-normalized metadata JSON.
Its `evidence` member is the RPC result object. Use it for interoperability
tests; its fixture authorizer is not a network trust configuration.

The existing Flashblock signing preimage does not include a chain ID. The returned
chain ID is RPC context, not a new cryptographic commitment. Network isolation
depends on independently choosing the correct trusted authorizer key; sharing a
key across networks does not gain replay protection from this response field.
Never bootstrap trust from a key supplied by the same untrusted RPC.

Signatures authenticate payload origin. Verifying individual logs additionally
requires receipt/transaction commitments and proofs; complete event coverage,
freshness and finality require their own checks. Registry-based key discovery is
a separate follow-up and does not change this API's existing signature format.

## Bounds and unavailable evidence

Each node retains at most 64 MiB of wire data and 512 metadata entries, for up to
60 seconds from first capture. Duplicate delivery and reads do not renew that
deadline. Expired data is released on subsequent capture/lookup. Each frame is
limited to 16 MiB; hexadecimal JSON can exceed 32 MiB with response metadata, so
RPC/proxy response limits must accommodate the desired payload size.

JSON-RPC error `-32080` includes the selector and a bounded `reason`:

| Reason | Meaning |
| --- | --- |
| `notRetained` | No retained capture/metadata: possibly never observed, restarted, or evicted. |
| `notOrdered` | Captured, but an earlier Flashblock is still missing. |
| `expired` | The known capture exceeded its retention deadline. |
| `evicted` | Wire bytes were evicted to respect the shared byte budget. |

There is no historical backfill or signature reconstruction. Another backend may
have a different capture window; it must not substitute another view for the
requested selector. Monitor `worldchain.evidence.captured`, `capture_rejections`,
`evictions`, `lookups`, `retained_bytes` and `retained_entries` with the
`worldchain.evidence.` prefix. Configure ordinary RPC concurrency/rate limits
before broad exposure; response serialization has a cost even for cached data.

## Receipt and transaction proofs

Opt in with `--rpc.flashblock-receipt-proofs` in addition to
`--rpc.flashblock-evidence`. On each HTTP or WebSocket listener that includes
`eth`, `op_supportedCapabilities` then includes
`worldchain_flashblock_receipt_proofs_v1`. The proof methods are unavailable on
IPC. Enable them on an intended listener or explicitly allowlisted proxy route.

Fetch the signed evidence and the proof using the same immutable selector. For
example, use the `payloadId`, `flashblockIndex` (as `index`), and `blockHash`
from the `view` of a preconfirmed-log response.

| Method | Positional parameters | V1 result |
| --- | --- | --- |
| `worldchain_getFlashblockReceiptProof` | `[id, transactionHash]` | `version`, `id`, RLP `header`, numeric `transactionIndex`, encoded `transaction` and `receipt`, and the `transactionProof` and `receiptProof` arrays of RLP nodes from root to leaf. |
| `worldchain_getFlashblockReceipts` | `[id]` | `version`, `id`, RLP `header`, and all encoded `receipts` in transaction order. |

Binary fields are hex strings. Transactions and receipts use their canonical
EIP-2718 trie bytes. OP deposit receipts reflect the active receipt-root rules,
including omission of the deposit nonce during Regolith before Canyon. Both trie
proofs use the same RLP-encoded transaction index. The consumer hashes the
header to match the signed block hash and checks the header receipt root against
the signed delta. The transaction hash is derived from the proved transaction
bytes.

The log commitment covers the address, ordered topics, data, and ordinal within
the receipt. An RPC cursor, event ID, global log index, and removal status are
feed metadata. Equal logs at different ordinals remain distinct. A single
inclusion proof authenticates the selected log. To check completeness, verify all
ordered receipts against the signed root and apply the filter locally. The
complete receipt result identifies receipts by index. Use the transaction proof
for a claimed transaction hash.

Save each JSON-RPC `result` object separately. Supply the expected view and
the separately trusted network authorizer key that was active when the frame was
signed. Keep previous public keys with their network and activation intervals to
verify archived evidence after rotation. The verifier requires an explicit key.

```sh
cargo run --locked -p world-chain-primitives --example verify-flashblock-receipts -- \
  log evidence.json proof.json "$TRUSTED_AUTHORIZER_HEX" "$CHAIN_ID" \
  "$PAYLOAD_ID" "$INDEX" "$BLOCK_HASH" "$TX_HASH" "$RECEIPT_ORDINAL" log.json

cargo run --locked -p world-chain-primitives --example verify-flashblock-receipts -- \
  complete evidence.json receipts.json "$TRUSTED_AUTHORIZER_HEX" "$CHAIN_ID" \
  "$PAYLOAD_ID" "$INDEX" "$BLOCK_HASH" "$ADDRESS" "$TOPIC0"
```

`log.json` contains only `address`, `topics`, and `data`. The
`complete` command prints every match for the supplied address and first topic
after verifying the full receipt root. Omission or reordering changes the root.

Proofs require both a retained signed frame and an execution-validated view. A
node can serve earlier indices of the same payload while it remains current.
A canonical head change or payload replacement can make an in-flight lookup
unavailable. Signed proof bundles can be verified offline after cache expiry or
a reorg. Check the full-chain receipt separately before treating a provisional
event as canonical or final.

There is a separate cache for proofs with up to 64 retained views, 64 MiB of
encoded data and index entries, and a 60-second lookup lifetime from validation.
One capture task encodes views. Each view is limited to 16,384 transactions and
8 MiB of total encoded header, transaction and receipt bytes. Up to two proof
requests perform trie work concurrently. Each response is bounded, including a
maximum of 8 MiB of binary data for complete receipts and a little over 16 MiB
for an inclusion proof with its transaction and receipt repeated in the nodes.
Allow 33 MiB of JSON response size to support the full proof and evidence
budgets. Smaller transport limits can reject otherwise valid large results.

Error `-32082` identifies the proof selector and a bounded reason. The evidence
availability reasons above also apply to proofs. Additional reasons include
`replaced` (the view left the current payload or canonical checkpoint),
`transactionNotFound`, `limitExceeded`, `rootMismatch`,
`inconsistentExecution`, `busy`, and `unavailable`. A `notRetained`
proof can also mean the proof worker has yet to capture that validated view.
