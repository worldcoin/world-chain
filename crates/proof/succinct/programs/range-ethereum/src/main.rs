//! SP1 range program for World Chain OP Succinct Lite fault proofs with Ethereum DA.

#![cfg_attr(target_os = "zkvm", no_main)]

#[cfg(target_os = "zkvm")]
sp1_zkvm::entrypoint!(main);

use rkyv::rancor::Error;
use world_chain_proof_core::witness::{WitnessData, WorldRangeWitnessData};
use world_chain_proof_succinct_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use world_chain_proof_succinct_range_utils::run_range_program;
#[cfg(feature = "tracing-subscriber")]
use world_chain_proof_succinct_range_utils::setup_tracing;

pub fn main() {
    #[cfg(feature = "tracing-subscriber")]
    setup_tracing();

    kona_proof::block_on(async move {
        let witness_rkyv_bytes: Vec<u8> = sp1_zkvm::io::read_vec();
        let witness_data =
            rkyv::from_bytes::<WorldRangeWitnessData, Error>(&witness_rkyv_bytes)
                .expect("failed to deserialize World range witness data");
        let world_schedule = witness_data.schedule.clone();
        let (oracle, beacon) = witness_data
            .get_oracle_and_blob_provider()
            .await
            .expect("failed to load oracle and blob provider");
        let boot_info = run_range_program(
            ETHDAWitnessExecutor::new(),
            oracle,
            beacon,
            world_schedule,
        )
        .await
        .expect("failed to hash World rollup config for boot info");

        sp1_zkvm::io::commit(&boot_info);
    });
}
