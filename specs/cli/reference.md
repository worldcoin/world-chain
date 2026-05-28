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

      --flashblocks.store
          Store accepted flashblocks to a separate libmdbx database
          
          [env: FLASHBLOCKS_STORE=]

      --flashblocks.store-path <STORE_PATH>
          Path to the libmdbx database directory used by --flashblocks.store.
          
          Defaults to <datadir>/flashblocks/flashblocks.mdbx.
          
          [env: FLASHBLOCKS_STORE_PATH=]

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

OP Proposer ExEx:
      --proposer.enabled
          Enable the OP Proposer ExEx
          
          [env: OP_PROPOSER_ENABLED=]

      --proposer.l1-eth-rpc <proposer_l1_eth_rpc>
          HTTP provider URL for L1
          
          [env: OP_PROPOSER_L1_ETH_RPC=]

      --proposer.rollup-rpc <proposer_rollup_rpc>
          Optional HTTP provider URL for a remote `op-node` rollup RPC.
          
          When set, the proposer will read output roots from this endpoint instead of computing them locally. A comma-separated list enables active failover (matches the Go behaviour).
          
          [env: OP_PROPOSER_ROLLUP_RPC=]

      --proposer.game-factory-address <proposer_game_factory_address>
          Address of the `DisputeGameFactory` contract
          
          [env: OP_PROPOSER_GAME_FACTORY_ADDRESS=]

      --proposer.poll-interval <proposer_poll_interval>
          Interval between checks for whether to load and submit a new proposal
          
          [env: OP_PROPOSER_POLL_INTERVAL=]
          [default: 12s]

      --proposer.proposal-interval <proposer_proposal_interval>
          Interval between submitting L2 output proposals
          
          [env: OP_PROPOSER_PROPOSAL_INTERVAL=]
          [default: 1h]

      --proposer.allow-non-finalized
          Allow proposals derived from non-finalized L1 data
          
          [env: OP_PROPOSER_ALLOW_NON_FINALIZED=]

      --proposer.game-type <proposer_game_type>
          Dispute game type
          
          [env: OP_PROPOSER_GAME_TYPE=]
          [default: 0]

      --proposer.active-sequencer-check-duration <proposer_active_sequencer_check_duration>
          Active sequencer check duration (only used with `--proposer.rollup-rpc`)
          
          [env: OP_PROPOSER_ACTIVE_SEQUENCER_CHECK_DURATION=]
          [default: 2m]

      --proposer.wait-node-sync
          Whether to wait for the node to sync to the current L1 tip before starting the driver loop
          
          [env: OP_PROPOSER_WAIT_NODE_SYNC=]

      --proposer.network-timeout <proposer_network_timeout>
          L1 network timeout (per-call)
          
          [env: OP_PROPOSER_NETWORK_TIMEOUT=]
          [default: 10s]

      --proposer.private-key <proposer_private_key>
          Hex-encoded private key of the L1 proposer EOA.
          
          Mutually exclusive with `--proposer.mnemonic`. Either is required when the proposer is enabled.
          
          [env: OP_PROPOSER_PRIVATE_KEY=]

      --proposer.mnemonic <proposer_mnemonic>
          BIP-39 mnemonic for the L1 proposer signer
          
          [env: OP_PROPOSER_MNEMONIC=]

      --proposer.hd-path <proposer_hd_path>
          HD derivation path used with `--proposer.mnemonic`
          
          [env: OP_PROPOSER_HD_PATH=]
          [default: m/44'/60'/0'/0/0]

      --proposer.balance-poll-interval <proposer_balance_poll_interval>
          Interval between wallet-balance metric refreshes
          
          [env: OP_PROPOSER_BALANCE_POLL_INTERVAL=]
          [default: 60s]

      --proposer.rpc-max-retries <proposer_rpc_max_retries>
          Maximum rate-limit retries the L1 transport will attempt before surfacing an error. `0` disables retries
          
          [env: OP_PROPOSER_RPC_MAX_RETRIES=]
          [default: 10]

      --proposer.rpc-initial-backoff-ms <proposer_rpc_initial_backoff_ms>
          Initial backoff for L1 rate-limit retries (ms)
          
          [env: OP_PROPOSER_RPC_INITIAL_BACKOFF_MS=]
          [default: 500]

      --proposer.rpc-cups <proposer_rpc_cups>
          Compute-units-per-second budget used by alloy's retry backoff layer to scale waits
          
          [env: OP_PROPOSER_RPC_CUPS=]
          [default: 660]

      --proposer.rpc-addr <proposer_rpc_addr>
          Admin RPC bind address
          
          [env: OP_PROPOSER_RPC_ADDR=]
          [default: 127.0.0.1]

      --proposer.rpc-port <proposer_rpc_port>
          Admin RPC bind port. Pass `0` to disable the admin RPC server
          
          [env: OP_PROPOSER_RPC_PORT=]
          [default: 0]

      --proposer.rpc-enable-admin
          Enable admin namespace on the proposer RPC
          
          [env: OP_PROPOSER_RPC_ENABLE_ADMIN=]

      --proposer.datadir <proposer_datadir>
          Directory for proposer persistent state (MDBX)
          
          [env: OP_PROPOSER_DATADIR=]

OP Batcher ExEx:
      --batcher.enabled
          Enable the OP Batcher ExEx
          
          [env: OP_BATCHER_ENABLED=]

      --batcher.l1-eth-rpc <batcher_l1_eth_rpc>
          HTTP provider URL(s) for L1 (comma-separated for failover)
          
          [env: OP_BATCHER_L1_ETH_RPC=]

      --batcher.batch-inbox-address <batcher_batch_inbox_address>
          Address of the L1 `BatchInbox` that batch transactions are sent to
          
          [env: OP_BATCHER_BATCH_INBOX_ADDRESS=]

      --batcher.poll-interval <batcher_poll_interval>
          Interval between batch-submission polling cycles
          
          [env: OP_BATCHER_POLL_INTERVAL=]
          [default: 6s]

      --batcher.max-l1-tx-size-bytes <batcher_max_l1_tx_size_bytes>
          Maximum L1 calldata tx size (bytes). The max frame size is this minus 1
          
          [env: OP_BATCHER_MAX_L1_TX_SIZE_BYTES=]
          [default: 120000]

      --batcher.max-channel-duration <batcher_max_channel_duration>
          Maximum number of L1 blocks a channel may remain open before being force-closed. `0` disables the duration timeout
          
          [env: OP_BATCHER_MAX_CHANNEL_DURATION=]
          [default: 0]

      --batcher.sub-safety-margin <batcher_sub_safety_margin>
          Number of L1 blocks subtracted from channel timeout / sequencing window when computing the close deadline, so the channel lands with margin
          
          [env: OP_BATCHER_SUB_SAFETY_MARGIN=]
          [default: 10]

      --batcher.approx-compr-ratio <batcher_approx_compr_ratio>
          Approximate compression ratio used to decide when a channel is full (input target = target output size / ratio)
          
          [env: OP_BATCHER_APPROX_COMPR_RATIO=]
          [default: 0.6]

      --batcher.channel-timeout <batcher_channel_timeout>
          On-chain channel timeout (L1 blocks). Defaults to the Granite value
          
          [env: OP_BATCHER_CHANNEL_TIMEOUT=]
          [default: 50]

      --batcher.wait-node-sync
          Whether to wait for the node to sync before starting the driver loop
          
          [env: OP_BATCHER_WAIT_NODE_SYNC=]

      --batcher.network-timeout <batcher_network_timeout>
          Allow batching against the unsafe head even when it is not yet safe (the normal mode). Reserved for future use; always true for v1
          
          [env: OP_BATCHER_NETWORK_TIMEOUT=]
          [default: 10s]

      --batcher.private-key <batcher_private_key>
          Hex-encoded private key of the L1 batcher EOA (must match `SystemConfig.batcherHash`). Mutually exclusive with `--batcher.mnemonic`
          
          [env: OP_BATCHER_PRIVATE_KEY=]

      --batcher.mnemonic <batcher_mnemonic>
          BIP-39 mnemonic for the L1 batcher signer
          
          [env: OP_BATCHER_MNEMONIC=]

      --batcher.hd-path <batcher_hd_path>
          HD derivation path used with `--batcher.mnemonic`
          
          [env: OP_BATCHER_HD_PATH=]
          [default: m/44'/60'/0'/0/0]

      --batcher.balance-poll-interval <batcher_balance_poll_interval>
          Interval between wallet-balance metric refreshes
          
          [env: OP_BATCHER_BALANCE_POLL_INTERVAL=]
          [default: 60s]

      --batcher.rpc-max-retries <batcher_rpc_max_retries>
          Maximum rate-limit retries the L1 transport will attempt. `0` disables
          
          [env: OP_BATCHER_RPC_MAX_RETRIES=]
          [default: 10]

      --batcher.rpc-initial-backoff-ms <batcher_rpc_initial_backoff_ms>
          Initial backoff for L1 rate-limit retries (ms)
          
          [env: OP_BATCHER_RPC_INITIAL_BACKOFF_MS=]
          [default: 500]

      --batcher.rpc-cups <batcher_rpc_cups>
          Compute-units-per-second budget used by alloy's retry backoff layer
          
          [env: OP_BATCHER_RPC_CUPS=]
          [default: 660]

      --batcher.rpc-addr <batcher_rpc_addr>
          Admin RPC bind address
          
          [env: OP_BATCHER_RPC_ADDR=]
          [default: 127.0.0.1]

      --batcher.rpc-port <batcher_rpc_port>
          Admin RPC bind port. Pass `0` to disable the admin RPC server
          
          [env: OP_BATCHER_RPC_PORT=]
          [default: 0]

      --batcher.rpc-enable-admin
          Enable the admin namespace on the batcher RPC
          
          [env: OP_BATCHER_RPC_ENABLE_ADMIN=]

      --batcher.datadir <batcher_datadir>
          Directory for batcher persistent state (MDBX)
          
          [env: OP_BATCHER_DATADIR=]

      --tx-peers <PEER_ID>
          Comma-separated list of peer IDs to which transactions should be propagated

      --worldchain.disable-bootnodes
          Disable the default World Chain bootnodes

```
