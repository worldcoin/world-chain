//! SP1 range program for World Chain OP Succinct Lite fault proofs.

#![cfg_attr(target_os = "zkvm", no_main)]

#[cfg(target_os = "zkvm")]
sp1_zkvm::entrypoint!(main);

use alloy_sol_types::SolValue;
use world_chain_proof_succinct_range_utils::{WorldRangeWitness, run_range_program};

pub fn main() {
    let witness = sp1_zkvm::io::read::<WorldRangeWitness>();
    let boot_info = run_range_program(witness).expect("failed to build World range public values");

    sp1_zkvm::io::commit_slice(&boot_info.abi_encode());
}
