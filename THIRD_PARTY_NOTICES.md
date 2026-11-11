# Third-party notices

## revm-inspectors native trace conversion

- Project: [revm-inspectors](https://github.com/paradigmxyz/revm-inspectors).
- Version: 0.43.0, published crate SHA-256
  `79b609cb78fcb3a8c10f3190847c9bb0223423cf46e2de30a61ea9e37a3e6736`.
- Exact source revision: `453c67d7ccdf51327c9e7687ac6ba0b8651e7f87`.
- Upstream paths:
  [src/tracing/builder/geth.rs](https://github.com/paradigmxyz/revm-inspectors/blob/453c67d7ccdf51327c9e7687ac6ba0b8651e7f87/src/tracing/builder/geth.rs),
  [src/tracing/types.rs](https://github.com/paradigmxyz/revm-inspectors/blob/453c67d7ccdf51327c9e7687ac6ba0b8651e7f87/src/tracing/types.rs),
  [src/tracing/utils.rs](https://github.com/paradigmxyz/revm-inspectors/blob/453c67d7ccdf51327c9e7687ac6ba0b8651e7f87/src/tracing/utils.rs).
- Local destination: `crates/rpc/src/bounded_trace.rs`.
- Status: modified/closely translated call/prestate/diff projection. Adds
  fallible traversal checkpoints, checked cumulative retention budgets,
  malformed-arena rejection, provider failure propagation and redaction;
  uses checked linear call assembly and per-account diff filtering.
- License expression: `MIT OR Apache-2.0`; this material is used under MIT.
  The upstream [LICENSE-MIT](https://github.com/paradigmxyz/revm-inspectors/blob/453c67d7ccdf51327c9e7687ac6ba0b8651e7f87/LICENSE-MIT)
  is retained verbatim in [third-party/revm-inspectors/LICENSE-MIT](third-party/revm-inspectors/LICENSE-MIT).
- Tracking: [BLO-1124](https://linear.app/worldcoin/issue/BLO-1124).

No upstream test body or unrelated formatter is copied. The existing Cargo
dependency versions remain pinned; this record concerns retained source
material and does not change this repository's LICENSE.
