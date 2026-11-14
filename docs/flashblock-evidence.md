# Flashblock signing evidence

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
