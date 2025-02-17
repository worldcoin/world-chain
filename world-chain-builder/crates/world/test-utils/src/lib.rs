//! Test utilities for interfacing with PBH & the World Chain Devnet.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_primitives::{address, Address};

const MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Devnet Test Safes
pub const TEST_SAFES: [Address; 6] = [
    address!("b35520e3D456b91FBEf36D8CB83fF7847937Ad26"),
    address!("52E5544fF7D6e1449E03Ade54a7fDc950Bb85CaB"),
    address!("D0968D7f1B3AEEB1904615d8c65C128682D2F457"),
    address!("48aA6040f470b8edeb23CD4B83dB1e7b45B7b530"),
    address!("68A8A253e27b42825E3d47Eb637433CAFf1B5310"),
    address!("58AF3b6E40603C9498fC28344DD2128a6F7FE510"),
];

/// Devnet Test 4337 Modules
pub const TEST_MODULES: [Address; 6] = [
    address!("a513E6E4b8f2a923D98304ec87F64353C4D5C853"),
    address!("610178dA211FEF7D417bC0e6FeD39F05609AD788"),
    address!("0DCd1Bf9A1b36cE34237eEaFef220932846BCD82"),
    address!("959922bE3CAee4b8Cd9a407cc3ac1C251C2007B1"),
    address!("3Aa5ebB10DC797CAC828524e59A333d0A371443c"),
    address!("4ed7c70F96B99c776995fB64377f0d4aB3B0e1C1"),
];

pub const DEV_CHAIN_ID: u64 = 2151908;

pub const PBH_NONCE_KEY: u32 = 1123123123;

pub const DEVNET_ENTRYPOINT: Address = address!("0000000071727De22E5E9d8BAf0edAc6f37da032");

pub const PBH_DEV_SIGNATURE_AGGREGATOR: Address =
    address!("Cf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9");

pub const PBH_DEV_ENTRYPOINT: Address = address!("9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");

pub const DEV_WORLD_ID: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");

pub mod bindings;
pub mod utils;
