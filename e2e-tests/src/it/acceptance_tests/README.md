# Acceptance Tests

The MVP checks:

- `eth_chainId` matches the configured chain ID
- `eth_getBlockByNumber("latest")` returns a block
- `eth_blockNumber` advances by at least `ACCEPTANCE_MIN_BLOCK_INCREMENTS`

## Environment

| Variable | Required in workflow | Default | Description |
| --- | --- | --- | --- |
| `ACCEPTANCE_RPC_URL` | yes in workflow | unset | RPC endpoint for the selected network, passed from the workflow matrix. |
| `ACCEPTANCE_CHAIN_ID` | yes when RPC URL is set | unset | Expected decimal chain ID passed by the workflow matrix or local shell. |
| `ACCEPTANCE_NETWORK` | no | `local` | Network label used in logs and error messages. |
| `CF_ACCESS_CLIENT_ID` | yes in workflow | unset | Cloudflare Access service token client ID. |
| `CF_ACCESS_CLIENT_SECRET` | yes in workflow | unset | Cloudflare Access service token client secret. |
| `ACCEPTANCE_BLOCK_ADVANCE_TIMEOUT_SECS` | no | `60` | Max time to wait for block-number progress. |
| `ACCEPTANCE_BLOCK_POLL_INTERVAL_SECS` | no | `2` | Poll interval while waiting for block-number progress. |
| `ACCEPTANCE_MIN_BLOCK_INCREMENTS` | no | `1` | Minimum required block-number increase. |

Example:

```sh
ACCEPTANCE_RPC_URL=https://example.invalid \
ACCEPTANCE_CHAIN_ID=4801 \
cargo nextest run --profile ci -p world-chain-tests \
  -E 'test(/acceptance_tests::test_network/)'
```
