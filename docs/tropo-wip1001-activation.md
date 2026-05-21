# Tropo WIP-1001 Activation

Tropo is the WIP-1001 activation boundary. WIP-1001 is active only when Tropo is active and a complete WIP-1001 activation parameter set is configured.

The exact production activation parameters are still under development. Until those values are finalized, the World mainnet and World Sepolia parameter constants intentionally stay unset.

## Configuration

- Tropo timing is fork-schedule metadata: `TROPO_UPGRADE_TIMESTAMP_MAINNET`, `TROPO_UPGRADE_TIMESTAMP_SEPOLIA`, or `tropoTime` in genesis for custom chain specs.
- WIP-1001 parameters are execution-layer chain-spec constants, not L1 contract values, rollup config values, or genesis extra fields.
- Devnets may opt into placeholder parameters with `--worldchain.enable-tropo-wip1001-placeholder`; tests may inject them with `WorldChainSpecBuilder::with_tropo_wip1001_parameters` or `WorldChainSpec::set_tropo_wip1001_parameters`.
- Production World chains must use the built-in network constants once finalized.

## Safety Rules

- Startup must fail if Tropo is scheduled but WIP-1001 parameters are unset.
- World mainnet and World Sepolia must reject dev placeholder or operator-selected parameter sets.
- After Tropo activates on a network, its WIP-1001 parameter set is immutable for that fork. Any later parameter change needs a new fork boundary.
- Block-gas-limit checks must use the current block gas limit in validation/execution code, not the genesis gas limit.

## Op-node

op-node needs to agree on the Tropo activation timestamp. It does not need the WIP-1001 parameter table unless the design changes and rollup config becomes the agreed carrier for additional fork metadata.

Transaction validation, pool admission, and execution consumption of these parameters are separate workstreams.
