# World Chain Vendoring Note

This directory vendors the Base Rust proof-system code used as architecture
reference for `wips/wip-1006.md`.

The compiled World Chain integration is intentionally first-party code under
`crates/proofs` and `crates/devnet`; this vendored tree is kept in-repo so the
proof architecture can be inspected without importing Base crates directly.
