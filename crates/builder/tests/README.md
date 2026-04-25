# Samply Profiling Tests

These tests are for profiling node's performances using `samply`.

## Requirements

1. Install [samply](https://github.com/mstange/samply)

## How to profile

To run profiling with `samply` you need to run:

```bash
samply record cargo test -p world-chain-builder --release --test flashblock_validation_synthetic_profile <TEST_NAME> -- --ignored --exact --nocapture
```

A real example:

```bash
samply record cargo test -p world-chain-builder --release --test flashblock_validation_synthetic_profile profile_process_flashblock_world_id_like_bn254_without_bal_50_txs -- --ignored --exact --nocapture
```
