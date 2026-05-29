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

Kona Consensus Node:
      --kona.enabled
          Enable the in-process Kona consensus node.
          
          When enabled, the world-chain binary acts as both the execution and consensus client.

      --kona.l1-rpc-url <kona.l1_rpc_url>
          L1 execution RPC URL for fetching deposits, batches, and finalization signals
          
          [env: KONA_L1_RPC_URL=]
          [default: http://localhost:8545]

      --kona.l1-beacon-url <kona.l1_beacon_url>
          L1 beacon API URL for fetching blob data (required post-Dencun)
          
          [env: KONA_L1_BEACON_URL=]
          [default: http://localhost:5052]

      --kona.l1-trust-rpc
          Trust the L1 RPC without additional receipt verification

Kona P2P:
      --p2p.no-discovery
          Disable Discv5 (node discovery)
          
          [env: KONA_NODE_P2P_NO_DISCOVERY=]

      --p2p.priv.path <kona.priv_path>
          Read the hex-encoded 32-byte private key for the peer ID from this txt file. Created if not already exists. Important to persist to keep the same network identity after restarting
          
          [env: KONA_NODE_P2P_PRIV_PATH=]

      --p2p.priv.raw <kona.private_key>
          The hex-encoded 32-byte private key for the peer ID
          
          [env: KONA_NODE_P2P_PRIV_RAW=]

      --p2p.advertise.ip <kona.advertise_ip>
          IP address or DNS hostname to advertise to external peers from Discv5. Uses `p2p.listen.ip` if not set. Setting this disables dynamic ENR updates
          
          [env: KONA_NODE_P2P_ADVERTISE_IP=]

      --p2p.advertise.tcp <kona.advertise_tcp_port>
          TCP port to advertise. Same as `p2p.listen.tcp` if not set
          
          [env: KONA_NODE_P2P_ADVERTISE_TCP_PORT=]

      --p2p.advertise.udp <kona.advertise_udp_port>
          UDP port to advertise. Same as `p2p.listen.udp` if not set
          
          [env: KONA_NODE_P2P_ADVERTISE_UDP_PORT=]

      --p2p.listen.ip <kona.listen_ip>
          IP address or DNS hostname to bind LibP2P/Discv5 to
          
          [env: KONA_NODE_P2P_LISTEN_IP=]
          [default: 0.0.0.0]

      --p2p.listen.tcp <kona.listen_tcp_port>
          TCP port to bind LibP2P to. Any available system port if set to 0
          
          [env: KONA_NODE_P2P_LISTEN_TCP_PORT=]
          [default: 9222]

      --p2p.listen.udp <kona.listen_udp_port>
          UDP port to bind Discv5 to. Same as TCP port if left 0
          
          [env: KONA_NODE_P2P_LISTEN_UDP_PORT=]
          [default: 9223]

      --p2p.peers.lo <kona.peers_lo>
          Low-tide peer count. The node actively searches for new peer connections if below this
          
          [env: KONA_NODE_P2P_PEERS_LO=]
          [default: 20]

      --p2p.peers.hi <kona.peers_hi>
          High-tide peer count. The node starts pruning peer connections after reaching this
          
          [env: KONA_NODE_P2P_PEERS_HI=]
          [default: 30]

      --p2p.peers.grace <kona.peers_grace>
          Grace period (seconds) to keep a newly connected peer around
          
          [env: KONA_NODE_P2P_PEERS_GRACE=]
          [default: 30]

      --p2p.gossip.mesh.d <kona.gossip_mesh_d>
          GossipSub topic stable mesh target count (desired outbound degree)
          
          [env: KONA_NODE_P2P_GOSSIP_MESH_D=]
          [default: 8]

      --p2p.gossip.mesh.lo <kona.gossip_mesh_dlo>
          GossipSub topic stable mesh low watermark
          
          [env: KONA_NODE_P2P_GOSSIP_MESH_DLO=]
          [default: 6]

      --p2p.gossip.mesh.dhi <kona.gossip_mesh_dhi>
          GossipSub topic stable mesh high watermark
          
          [env: KONA_NODE_P2P_GOSSIP_MESH_DHI=]
          [default: 12]

      --p2p.gossip.mesh.dlazy <kona.gossip_mesh_dlazy>
          GossipSub gossip target (announcements of IHAVE)
          
          [env: KONA_NODE_P2P_GOSSIP_MESH_DLAZY=]
          [default: 6]

      --p2p.gossip.mesh.floodpublish
          Publish messages to all known peers on the topic, outside of the mesh
          
          [env: KONA_NODE_P2P_GOSSIP_FLOOD_PUBLISH=]

      --p2p.scoring <kona.scoring>
          Peer scoring strategy: none or light
          
          [env: KONA_NODE_P2P_SCORING=]
          [default: light]

      --p2p.ban.peers
          Ban peers based on their score
          
          [env: KONA_NODE_P2P_BAN_PEERS=]

      --p2p.ban.threshold <kona.ban_threshold>
          Score threshold below which peers are banned
          
          [env: KONA_NODE_P2P_BAN_THRESHOLD=]
          [default: -100]

      --p2p.ban.duration <kona.ban_duration>
          Duration in minutes to ban a peer for
          
          [env: KONA_NODE_P2P_BAN_DURATION=]
          [default: 60]

      --p2p.discovery.interval <kona.discovery_interval>
          Interval in seconds to find peers using the discovery service
          
          [env: KONA_NODE_P2P_DISCOVERY_INTERVAL=]
          [default: 5]

      --p2p.discovery.randomize <kona.discovery_randomize>
          Seconds to wait before removing a random peer from discovery to rotate the peer set
          
          [env: KONA_NODE_P2P_DISCOVERY_RANDOMIZE=]

      --p2p.bootstore <kona.bootstore>
          Directory to store the bootstore
          
          [env: KONA_NODE_P2P_BOOTSTORE=]

      --p2p.no-bootstore
          Disable the bootstore
          
          [env: KONA_NODE_P2P_NO_BOOTSTORE=]

      --p2p.redial <kona.peer_redial>
          Max redial attempts for a disconnected peer. 0 = unlimited
          
          [env: KONA_NODE_P2P_REDIAL=]
          [default: 500]

      --p2p.redial.period <kona.redial_period>
          Duration in minutes of the peer dial period
          
          [env: KONA_NODE_P2P_REDIAL_PERIOD=]
          [default: 60]

      --p2p.bootnodes <kona.bootnodes>
          Comma-separated list of bootnode ENRs or enode URLs
          
          [env: KONA_NODE_P2P_BOOTNODES=]

      --p2p.topic-scoring
          Enable topic scoring (being phased out, for backwards-compat/debugging only)
          
          [env: KONA_NODE_P2P_TOPIC_SCORING=]

      --p2p.unsafe.block.signer <kona.unsafe_block_signer>
          Override the unsafe block signer address. By default fetched from rollup config's system config on L1
          
          [env: KONA_NODE_P2P_UNSAFE_BLOCK_SIGNER=]

      --p2p.sequencer.key <kona.sequencer_key>
          Local private key for the sequencer to sign unsafe blocks
          
          [env: KONA_NODE_P2P_SEQUENCER_KEY=]

      --p2p.sequencer.key.path <kona.sequencer_key_path>
          Path to a file containing the sequencer private key
          
          [env: KONA_NODE_P2P_SEQUENCER_KEY_PATH=]

      --p2p.signer.endpoint <kona.endpoint>
          URL of the remote signer endpoint
          
          [env: KONA_NODE_P2P_SIGNER_ENDPOINT=]

      --p2p.signer.address <kona.address>
          Address to sign transactions for (required with remote signer)
          
          [env: KONA_NODE_P2P_SIGNER_ADDRESS=]

      --p2p.signer.header <kona.header>
          Headers for the remote signer. Format: `key=value`
          
          [env: KONA_NODE_P2P_SIGNER_HEADER=]

      --p2p.signer.tls.ca <kona.ca_cert>
          Path to CA certificates for the remote signer
          
          [env: KONA_NODE_P2P_SIGNER_TLS_CA=]

      --p2p.signer.tls.cert <kona.cert>
          Path to the client certificate for the remote signer
          
          [env: KONA_NODE_P2P_SIGNER_TLS_CERT=]

      --p2p.signer.tls.key <kona.key>
          Path to the client key for the remote signer
          
          [env: KONA_NODE_P2P_SIGNER_TLS_KEY=]

      --kona.rollup-config <kona.rollup_config_path>
          Path to the OP Stack rollup configuration JSON file.
          
          This file defines the rollup parameters (chain ID, block time, hardfork activation timestamps, genesis hashes, etc.) used by the Kona consensus node. It follows the same format as op-node's `--rollup.config` flag.
          
          [env: KONA_ROLLUP_CONFIG=]

      --kona.sequencer
          Run the Kona consensus node in sequencer mode.
          
          When set, the node builds and gossips unsafe blocks rather than only following the chain.
          
          [env: KONA_SEQUENCER=]

      --kona.sequencer.stopped
          Start the sequencer in the stopped state.
          
          Block production must be resumed explicitly (e.g. via the admin RPC or op-conductor).
          
          [env: KONA_SEQUENCER_STOPPED=]

      --kona.sequencer.recover
          Run the sequencer in recovery mode
          
          [env: KONA_SEQUENCER_RECOVER=]

      --kona.sequencer.l1-confs <kona.l1_confs>
          Number of L1 confirmations the sequencer waits on before building from an L1 origin
          
          [env: KONA_SEQUENCER_L1_CONFS=]
          [default: 4]

      --kona.conductor.rpc <kona.conductor_rpc>
          URL of the op-conductor RPC endpoint. When set, the conductor service is enabled
          
          [env: KONA_CONDUCTOR_RPC=]

      --kona.rpc.addr <kona.rpc_addr>
          IP address the Kona node RPC server binds to
          
          [env: KONA_RPC_ADDR=]
          [default: 0.0.0.0]

      --kona.rpc.port <kona.rpc_port>
          Port the Kona node RPC server binds to
          
          [env: KONA_RPC_PORT=]
          [default: 8547]

      --kona.rpc.enable-admin
          Enable the admin namespace on the Kona node RPC server
          
          [env: KONA_RPC_ENABLE_ADMIN=]

      --kona.rpc.disabled
          Disable the Kona node RPC server entirely
          
          [env: KONA_RPC_DISABLED=]

      --kona.l1-slot-duration-override <kona.l1_slot_duration_override>
          Override the L1 slot duration (in seconds) used by the L1 watcher
          
          [env: KONA_L1_SLOT_DURATION_OVERRIDE=]

      --tx-peers <PEER_ID>
          Comma-separated list of peer IDs to which transactions should be propagated

      --worldchain.disable-bootnodes
          Disable the default World Chain bootnodes

```
