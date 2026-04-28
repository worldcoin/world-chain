# CLI Reference

> **Auto-generated** — run `cargo xtask docs` to regenerate.

## `world-chain`

The World Chain node binary. All flags below are passed to `world-chain`.

```text
World Chain Node

Usage: world-chain [OPTIONS]

Options:
  -h, --help
          Print help (see a summary with '-h')

Rollup:
      --rollup.sequencer <SEQUENCER>
          Endpoint for the sequencer mempool (can be both HTTP and WS)
          
          [aliases: --rollup.sequencer-http, --rollup.sequencer-ws]

      --rollup.disable-tx-pool-gossip
          Disable transaction pool gossip

      --rollup.compute-pending-block
          By default the pending block equals the latest block to save resources and not leak txs from the tx-pool, this flag enables computing of the pending block from the tx-pool instead.
          
          If `compute_pending_block` is not enabled, the payload builder will use the payload attributes from the latest block. Note that this flag is not yet functional.

      --rollup.discovery.v4
          enables discovery v4 if provided

      --rollup.enable-tx-conditional
          Enable transaction conditional support on sequencer

      --rollup.supervisor-http <SUPERVISOR_HTTP_URL>
          HTTP endpoint for the supervisor. When not set, interop transaction validation is disabled

      --rollup.supervisor-safety-level <SUPERVISOR_SAFETY_LEVEL>
          Safety level for the supervisor
          
          [default: CrossUnsafe]

      --rollup.sequencer-headers <SEQUENCER_HEADERS>
          Optional headers to use when connecting to the sequencer

      --rollup.historicalrpc <HISTORICAL_HTTP_URL>
          RPC endpoint for historical data

      --min-suggested-priority-fee <MIN_SUGGESTED_PRIORITY_FEE>
          Minimum suggested priority fee (tip) in wei, default `1_000_000`
          
          [default: 1000000]

      --flashblocks-url <FLASHBLOCKS_URL>
          A URL pointing to a secure websocket subscription that streams out flashblocks.
          
          If given, the flashblocks are received to build pending block. All request with "pending" block tag will use the pending state based on flashblocks.

      --flashblock-consensus
          Enable flashblock consensus client to drive the chain forward
          
          When enabled, the flashblock consensus client will process flashblock sequences and submit them to the engine API to advance the chain. Requires `flashblocks_url` to be set.

      --proofs-history
          If true, initialize external-proofs exex to save and serve trie nodes to provide proofs faster

      --proofs-history.storage-path <PROOFS_HISTORY_STORAGE_PATH>
          The path to the storage DB for proofs history

      --proofs-history.window <PROOFS_HISTORY_WINDOW>
          The window to span blocks for proofs history. Value is the number of blocks. Default is 1 month of blocks based on 2 seconds block time. 30 * 24 * 60 * 60 / 2 = `1_296_000`
          
          [default: 1296000]

      --proofs-history.prune-interval <PROOFS_HISTORY_PRUNE_INTERVAL>
          Interval between proof-storage prune runs. Accepts human-friendly durations like "100s", "5m", "1h". Defaults to 15s.
          
          - Shorter intervals prune smaller batches more often, so each prune run tends to be faster and the blocking pause for writes is shorter, at the cost of more frequent pauses. - Longer intervals prune larger batches less often, which reduces how often pruning runs, but each run can take longer and block writes for longer.
          
          A shorter interval is preferred so that prune runs stay small and don’t stall writes for too long.
          
          CLI: `--proofs-history.prune-interval 10m`
          
          [default: 15s]

      --proofs-history.verification-interval <PROOFS_HISTORY_VERIFICATION_INTERVAL>
          Verification interval: perform full block execution every N blocks for data integrity. - 0: Disabled (Default) (always use fast path with pre-computed data from notifications) - 1: Always verify (always execute blocks, slowest) - N: Verify every Nth block (e.g., 100 = every 100 blocks)
          
          Periodic verification helps catch data corruption or consensus bugs while maintaining good performance.
          
          CLI: `--proofs-history.verification-interval 100`
          
          [default: 0]

Priority Blockspace for Humans:
      --pbh.verified-blockspace-capacity <VERIFIED_BLOCKSPACE_CAPACITY>
          Sets the max blockspace reserved for verified transactions. If there are not enough verified transactions to fill the capacity, the remaining blockspace will be filled with unverified transactions. This arg is a percentage of the total blockspace with the default set to 70 (ie 70%)
          
          [default: 70]

      --pbh.entrypoint <ENTRYPOINT>
          Sets the ERC-4337 EntryPoint Proxy contract address This contract is used to validate 4337 PBH bundles
          
          [default: 0x0000000000000000000000000000000000000000]

      --pbh.world-id <WORLD_ID>
          Sets the WorldID contract address. This contract is used to provide the latest merkle root on chain
          
          [default: 0x0000000000000000000000000000000000000000]

      --pbh.signature-aggregator <SIGNATURE_AGGREGATOR>
          Sets the ERC0-7766 Signature Aggregator contract address This contract signifies that a given bundle should receive priority inclusion if it passes validation
          
          [default: 0x0000000000000000000000000000000000000000]

Block Builder:
      --builder.enabled
          

      --builder.private-key <PRIVATE_KEY>
          Private key for the builder used to update PBH nullifiers
          
          [env: BUILDER_PRIVATE_KEY=]
          [default: 0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef]

      --builder.block-uncompressed-size-limit <BLOCK_UNCOMPRESSED_SIZE_LIMIT>
          Maximum cumulative uncompressed (EIP-2718 encoded) block size in bytes
          
          [env: BUILDER_BLOCK_UNCOMPRESSED_SIZE_LIMIT=]

Flashblocks:
      --flashblocks.enabled
          

      --flashblocks.authorizer-vk <AUTHORIZER_VK>
          Authorizer verifying key used to verify flashblock authenticity
          
          [env: FLASHBLOCKS_AUTHORIZER_VK=]

      --flashblocks.builder-sk <BUILDER_SK>
          Flashblocks signing key used to sign authorized flashblocks payloads
          
          [env: FLASHBLOCKS_BUILDER_SK=]

      --flashblocks.override-authorizer-sk <OVERRIDE_AUTHORIZER_SK>
          Override incoming authorizations from rollup boost
          
          [env: FLASHBLOCKS_OVERRIDE_AUTHORIZER_SK=]

      --flashblocks.force-publish
          Publish flashblocks payloads even when an authorization has not been received from rollup boost.
          
          This should only be used for testing and development purposes.
          
          [env: FLASHBLOCKS_FORCE_PUBLISH=]

      --flashblocks.interval <FLASHBLOCKS_INTERVAL>
          The interval to publish pre-confirmations when building a payload in milliseconds
          
          [env: FLASHBLOCKS_INTERVAL=]
          [default: 200]

      --flashblocks.recommit-interval <RECOMMIT_INTERVAL>
          Interval at which the block builder should re-commit to the transaction pool when building a payload.
          
          In milliseconds.
          
          [env: FLASHBLOCKS_RECOMMIT_INTERVAL=]
          [default: 200]

      --flashblocks.access-list
          Enables flashblocks access list support.
          
          Will create access lists when building flashblocks payloads. and will use access lists for parallel transaction execution when verifying flashblocks payloads.
          
          [env: FLASHBLOCKS_ACCESS_LIST=]

      --flashblocks.max-send-peers <MAX_SEND_PEERS>
          Override the flashblocks send-set size
          
          [env: FLASHBLOCKS_MAX_SEND_PEERS=]
          [default: 10]

      --flashblocks.max-receive-peers <MAX_RECEIVE_PEERS>
          Override the number of receive peers maintained for flashblocks fanout
          
          [env: FLASHBLOCKS_MAX_RECEIVE_PEERS=]
          [default: 3]

      --flashblocks.rotation-interval <ROTATION_INTERVAL>
          Override the flashblocks rotation interval in seconds
          
          [env: FLASHBLOCKS_ROTATION_INTERVAL=]
          [default: 30]

      --flashblocks.score-samples <SCORE_SAMPLES>
          Override the number of latency samples retained for receive-peer scoring
          
          [env: FLASHBLOCKS_SCORE_SAMPLES=]
          [default: 1000]

      --flashblocks.force-receive-peers <PEER_ID>
          Peers to always receive flashblocks from regardless of their score.
          
          These peers will be requested as soon as they connect and will never be evicted by rotation. They count toward `max_receive_peers`.
          
          [env: FLASHBLOCKS_FORCE_RECEIVE_PEERS=]

      --tx-peers <PEER_ID>
          Comma-separated list of peer IDs to which transactions should be propagated

      --worldchain.disable-bootnodes
          Disable the default World Chain bootnodes

```
