# Acceptance Tests

The MVP checks:

- `eth_chainId` matches the configured chain ID
- `eth_getBlockByNumber("latest")` returns a block
- `eth_blockNumber` advances by at least `ACCEPTANCE_MIN_BLOCK_INCREMENTS`
- when `ACCEPTANCE_KARST_ENABLED=true`, Karst L2 execution checks call
  `eth_config` and send funded EOA transactions covering MODEXP bounds/gas
  floors, P256VERIFY gas floor, bn256 pairing input limit, CLZ, and transaction
  gas cap rejection
- when `ACCEPTANCE_KARST_DEPOSIT_ENABLED=true`, the Karst checks also submit an
  L1 portal deposit whose derived L2 deposit gas limit is above the Karst
  transaction gas cap, then require the L2 deposit receipt to land
- when `ACCEPTANCE_BUNDLER_RPC_URL` is set, the configured sponsored Rundler endpoint accepts ERC-4337 v0.7 user operations for ephemeral Safe smart account wallets
- the sponsored ERC-4337 checks cover concurrent wallet deployment, parallel post-deploy user operations across every wallet, multi-lane 2D nonce bursts, replay/gap rejection, and sponsorship constraint rejection

## Environment

| Variable | Required in workflow | Default | Description |
| --- | --- | --- | --- |
| `ACCEPTANCE_RPC_URL` | yes in workflow | unset | RPC endpoint for the selected network, passed from the workflow matrix. |
| `ACCEPTANCE_CHAIN_ID` | yes when RPC URL is set | unset | Expected decimal chain ID passed by the workflow matrix or local shell. |
| `ACCEPTANCE_NETWORK` | no | `local` | Network label used in logs and error messages. |
| `CF_ACCESS_CLIENT_ID` | yes in workflow | unset | Cloudflare Access service token client ID. |
| `CF_ACCESS_CLIENT_SECRET` | yes in workflow | unset | Cloudflare Access service token client secret. |
| `ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_ID` | yes in workflow when the bundler URL uses a separate Access app | `CF_ACCESS_CLIENT_ID` | Bundler-specific Cloudflare Access service token client ID. |
| `ACCEPTANCE_BUNDLER_CF_ACCESS_CLIENT_SECRET` | yes in workflow when the bundler URL uses a separate Access app | `CF_ACCESS_CLIENT_SECRET` | Bundler-specific Cloudflare Access service token client secret. |
| `ACCEPTANCE_BLOCK_ADVANCE_TIMEOUT_SECS` | no | `60` | Max time to wait for block-number progress. |
| `ACCEPTANCE_BLOCK_POLL_INTERVAL_SECS` | no | `2` | Poll interval while waiting for block-number progress. |
| `ACCEPTANCE_MIN_BLOCK_INCREMENTS` | no | `1` | Minimum required block-number increase. |
| `ACCEPTANCE_TX_TIMEOUT_SECS` | no | `60` | Max time to wait for an acceptance test L2 transaction receipt. |
| `ACCEPTANCE_TX_POLL_INTERVAL_MS` | no | `500` | Poll interval while waiting for acceptance test L2 transaction receipts. |
| `ACCEPTANCE_L2_KEY` | no | fallback development key | Funded L2 EOA key used by acceptance checks that submit L2 transactions. |
| `ACCEPTANCE_KARST_ENABLED` | no | `false` | Enables Karst L2 execution checks. |
| `ACCEPTANCE_KARST_DEPOSIT_ENABLED` | no | `false` | Enables the Karst L1 deposit bypass check. Requires `ACCEPTANCE_KARST_ENABLED=true`. |
| `ACCEPTANCE_L1_RPC_URL` | yes when Karst deposit checks are enabled | unset | L1 RPC endpoint used to submit the OptimismPortal deposit transaction. |
| `ACCEPTANCE_L1_KEY` | yes when Karst deposit checks are enabled | unset | Funded L1 EOA key used to pay L1 gas for the OptimismPortal deposit transaction. |
| `ACCEPTANCE_OPTIMISM_PORTAL` | yes when Karst deposit checks are enabled | unset | L1 OptimismPortal proxy address. |
| `ACCEPTANCE_BUNDLER_RPC_URL` | no | unset | Rundler RPC endpoint. If unset, ERC-4337 checks are skipped. |
| `ACCEPTANCE_4337_RPC_URL` | no | `ACCEPTANCE_RPC_URL` | Execution RPC used for ERC-4337 contract reads when the configured Rundler targets a different chain than the rest of the acceptance suite. |
| `ACCEPTANCE_4337_ENTRY_POINT` | no on chain `69420`; yes otherwise | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` on chain `69420` | ERC-4337 v0.7 EntryPoint address. |
| `ACCEPTANCE_4337_MODULE` | no on chain `69420`; yes otherwise | `0x70673A08a5B1086585d39979Fb2d84FDC0bB6Aaf` on chain `69420` | Safe4337 module used to sign Safe user operations. |
| `ACCEPTANCE_4337_WALLET_DEPLOYER` | no on chain `69420`; yes otherwise | `0xd1f0B51940DbD6e73891D2a41Ef14483fDC5Cb6e` on chain `69420` | Safe4337 wallet deployer used as the v0.7 factory. |
| `ACCEPTANCE_4337_PROFILE` | no | unset | Named user-operation profile. Supported values: `heavy`, `smoke`. Explicit per-knob env vars still override the profile. |
| `ACCEPTANCE_4337_WALLET_COUNT` | no | `20` | Number of ephemeral Safe wallets to deploy through sponsored user operations. |
| `ACCEPTANCE_4337_DEPLOY_CONCURRENCY` | no | `10` | Max number of wallet-deploying user operations sent and awaited concurrently. Kept at Rundler's unstaked-factory in-flight limit because all deploy ops use the same Safe factory. |
| `ACCEPTANCE_4337_OPS_PER_WALLET` | no | `3` | Number of post-deploy sponsored no-op user operations to send for each wallet. |
| `ACCEPTANCE_4337_OP_CONCURRENCY` | no | `60` | Max number of post-deploy sponsored no-op user operations sent and awaited concurrently. |
| `ACCEPTANCE_4337_NONCE_CONCURRENCY` | no | `2` | Max number of same-wallet 2D nonce checks sent and awaited concurrently. |
| `ACCEPTANCE_4337_OWNER_START_INDEX` | no | `1000` | First deterministic mnemonic account index used as a Safe owner. |
| `ACCEPTANCE_USEROP_TIMEOUT_SECS` | no | `30` | Max time to wait for a sent user operation receipt. |
| `ACCEPTANCE_USEROP_REJECT_TIMEOUT_SECS` | no | `3` | Max time to wait for a sad-path user operation to be rejected. |
| `ACCEPTANCE_USEROP_POLL_INTERVAL_MS` | no | `250` | Poll interval while waiting for a user operation receipt. |

The ERC-4337 wallet owners are deterministic test mnemonic accounts and are only
used to sign Safe user operations. They do not need ETH. The tests send standard
two-parameter `eth_sendUserOperation` requests; sponsorship is expected to be
injected by the configured Rundler endpoint. The ERC-4337 signing chain ID is
read from Rundler. Contract reads use `ACCEPTANCE_RPC_URL` unless
`ACCEPTANCE_4337_RPC_URL` is set; when set, the override RPC chain ID must match
Rundler's chain ID.

The GitHub Actions acceptance workflow sets `ACCEPTANCE_4337_PROFILE=smoke` for
bundler-enabled runs. That profile means 20 ephemeral Safe wallets, 2 post-deploy
sponsored no-op operations per wallet, and 20-way post-deploy operation
concurrency. Deploy concurrency stays at 10 because all deployment operations use
the same Safe factory.

Example:

```sh
ACCEPTANCE_RPC_URL=https://example.invalid \
ACCEPTANCE_CHAIN_ID=4801 \
cargo nextest run --profile ci -p world-chain-tests \
  -E 'test(/acceptance_tests::test_network/)'
```

Example with Rundler checks enabled against chain `69420`:

```sh
ACCEPTANCE_RPC_URL=https://tx-proxy-devnet-eu-central-2.worldcoin.dev \
ACCEPTANCE_BUNDLER_RPC_URL=https://rundler-devnet-eu-central-2.worldcoin.dev \
ACCEPTANCE_CHAIN_ID=69420 \
cargo nextest run --profile ci -p world-chain-tests \
  -E 'test(/acceptance_tests::test_network/)'
```

Example with Karst L2 checks enabled against a local devnet:

```sh
ACCEPTANCE_RPC_URL="$(jq -r .primary.l2_rpc_url target/devnet/endpoints.json)" \
ACCEPTANCE_CHAIN_ID="$(jq -r .chain_id target/devnet/endpoints.json)" \
ACCEPTANCE_KARST_ENABLED=true \
ACCEPTANCE_L2_KEY=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d \
cargo nextest run --profile ci -p world-chain-tests \
  -E 'test(/acceptance_tests::test_network/)'
```

Example with Karst L1 deposit bypass checks enabled against a local devnet:

```sh
ACCEPTANCE_RPC_URL="$(jq -r .primary.sequencer_rpc_url target/devnet/endpoints.json)" \
ACCEPTANCE_CHAIN_ID="$(jq -r .chain_id target/devnet/endpoints.json)" \
ACCEPTANCE_KARST_ENABLED=true \
ACCEPTANCE_L2_KEY=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d \
ACCEPTANCE_KARST_DEPOSIT_ENABLED=true \
ACCEPTANCE_L1_RPC_URL="$(jq -r .primary.l1_rpc_url target/devnet/endpoints.json)" \
ACCEPTANCE_L1_KEY=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d \
ACCEPTANCE_OPTIMISM_PORTAL="$(jq -r .primary.optimism_portal target/devnet/endpoints.json)" \
cargo nextest run --profile ci -p world-chain-tests \
  -E 'test(/acceptance_tests::test_network/)'
```
