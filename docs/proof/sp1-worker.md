# SP1 worker

`world-chain-proof-sp1-worker` has separate commands for running the worker and funding its
Succinct proving-network account.

## Run the worker

The existing worker arguments follow the `run` command:

```bash
world-chain-proof-sp1-worker run \
  --prover-service-url http://prover-service:8545 \
  --l1-rpc "$L1_RPC_URL" \
  --l1-beacon-rpc "$L1_BEACON_RPC_URL" \
  --l2-rpc "$L2_RPC_URL" \
  --block-interval 10 \
  --worker-id worker-0 \
  --prover network
```

The network prover additionally requires:

- `SP1_PRIVATE_KEY`: signs SP1 proof requests and identifies the credited account.
- `SP1_NETWORK_L1_RPC_URL`: Ethereum mainnet RPC used for Succinct settlement reads.
- `SUCCINCT_VAPP_ADDRESS`: SuccinctVApp proxy address on Ethereum mainnet.

At startup the worker validates that the settlement RPC is Ethereum mainnet, discovers the PROVE
token and `minDepositAmount()` from the configured VApp, then waits until the account has at least
`10 * minDepositAmount()` in SP1 Network credits. It retries the credit check every 30 seconds and
does not lease jobs while waiting. Once running, a background check updates
`sp1_network_prove_balance` and `sp1_network_balance_sufficient`; a later low balance is logged but
does not interrupt in-flight work.

`NETWORK_RPC_URL` remains the optional override for the SP1 Network API itself. It is distinct from
`SP1_NETWORK_L1_RPC_URL`.

## Deposit PROVE

The funding command signs an EIP-2612 permit and submits one `permitAndDeposit` transaction. Prefer
providing the key and settlement configuration through environment variables backed by your secret
store:

```bash
# SP1_NETWORK_L1_RPC_URL, SUCCINCT_VAPP_ADDRESS, and SP1_PRIVATE_KEY are injected.
world-chain-proof-sp1-worker deposit --amount 1000
```

The amount is human-readable PROVE; token decimals are read on-chain. The command validates the
Ethereum mainnet contracts, the signer's PROVE and ETH balances, waits for a successful transaction
receipt, prints the transaction hash and Succinct receipt ID, then polls until the SP1 Network
credit balance increases. If credits have not changed within 30 minutes, it exits with an error
that retains the successful transaction hash and receipt ID for investigation.
