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
  --worker-id worker-0 \
  --prover network
```

For every leased job, the worker reads the proof interval and immutable transition metadata from
the job's `MultiProofGame`. It rejects queued root, block, L1 head, or rollup-config values that do
not match the game before collecting a witness.

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

### SP1 Network request configuration

Network requests skip local guest execution by default and submit with separate upper bounds for
the range and aggregation guests:

| Variable | Flag | Default |
|---|---|---:|
| `SP1_RANGE_CYCLE_LIMIT` | `--sp1-range-cycle-limit` | `1500000000000` |
| `SP1_RANGE_GAS_LIMIT` | `--sp1-range-gas-limit` | `1300000000000` PGUs |
| `SP1_AGGREGATION_CYCLE_LIMIT` | `--sp1-aggregation-cycle-limit` | `7000000` |
| `SP1_AGGREGATION_GAS_LIMIT` | `--sp1-aggregation-gas-limit` | `6500000` PGUs |
| `SP1_MAX_PRICE_PER_PGU` | `--sp1-max-price-per-pgu` | SP1 Network default |
| `SP1_AUCTION_TIMEOUT_SECONDS` | `--sp1-auction-timeout-seconds` | SP1 SDK default (30 seconds) |
| `SP1_PROOF_TIMEOUT_SECONDS` | `--sp1-proof-timeout-seconds` | SP1 SDK derived deadline |

These are execution safety ceilings, not the final auction charge. The gas limit still affects the
request's worst-case authorization and balance check because the network multiplies it by the
maximum price per PGU. To execute each guest locally and let the SP1 SDK estimate both limits
instead, set `SP1_ESTIMATE_LIMITS=true` or pass `--sp1-estimate-limits`. Local estimation conflicts
with explicitly configured limit flags.

`SP1_MAX_PRICE_PER_PGU` caps the auction price encoded in each range and aggregation request. The
value uses PROVE base units (18 decimals) per PGU. For example, `50000000` is `0.05 PROVE/bPGU`.
When omitted, the SP1 SDK uses the maximum price returned by the Succinct Network RPC.

The auction timeout limits how long a request may remain unassigned. The proof timeout sets the
request's overall network deadline and may be longer than four hours when configured explicitly;
when omitted, the SDK derives a deadline from the gas limit and caps it at four hours.

If either phase times out, the worker marks that request failed and resubmits after one, two, and
five minutes. This three-resubmission budget applies to one worker attempt. The prover service
separately bounds complete worker attempts, so a restarted or expired lease cannot retry forever.
Completed range proofs are reused when only aggregation needs to be retried.

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
