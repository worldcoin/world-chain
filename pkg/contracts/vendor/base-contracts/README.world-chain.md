# World Chain Vendoring Note

This directory vendors the Base contracts repository for the proof-system
architecture referenced by `wips/wip-1006.md`.

The World Chain implementation lives in `pkg/contracts/src/proofs`. The vendored
Base source is kept in-tree for reviewability and to avoid importing proof
contracts through a submodule or package dependency.
