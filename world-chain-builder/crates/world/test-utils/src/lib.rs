//! Test utilities for interfacing with PBH & the World Chain Devnet.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_primitives::{address, Address};

const MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Devnet Test Safes
pub const TEST_SAFES: [Address; 6] = [
    address!("26479c9A59462f08f663281df6098dF7e7398363"),
    address!("CaD130298688716cDd16511C18DD88CAc327B3cA"),
    address!("4DFE55bC8eEa517efdbb6B2d056135b350b79ca2"),
    address!("e27eA3E8D654a0348DE1B42983F75c9594CdB2a2"),
    address!("220e1F32E1a76093C34388D4EeEB5fc0cAc73Ffb"),
    address!("e98F083C8C5e43593A517dA7b6b83Cb64F93c740"),
];

/// Devnet Test 4337 Modules
pub const TEST_MODULES: [Address; 6] = [
    address!("8A791620dd6260079BF849Dc5567aDC3F2FdC318"),
    address!("A51c1fc2f0D1a1b8494Ed1FE312d7C3a78Ed91C0"),
    address!("0B306BF915C4d645ff596e518fAf3F9669b97016"),
    address!("68B1D87F95878fE05B998F19b66F4baba5De1aed"),
    address!("59b670e9fA9D0A427751Af201D676719a970857b"),
    address!("a85233C63b9Ee964Add6F2cffe00Fd84eb32338f"),
];

pub const DEV_CHAIN_ID: u64 = 2151908;

pub const PBH_NONCE_KEY: u32 = 1123123123;

pub const DEVNET_ENTRYPOINT: Address = address!("9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");

pub const PBH_DEV_SIGNATURE_AGGREGATOR: Address =
    address!("5FC8d32690cc91D4c39d9d3abcBD16989F875707");

pub const PBH_DEV_ENTRYPOINT: Address = address!("Dc64a140Aa3E981100a9becA4E685f962f0cF6C9");

pub const DEV_WORLD_ID: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");

pub mod bindings;
pub mod utils;
