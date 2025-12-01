//! Test utilities for interfacing with PBH & the World Chain Devnet.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_primitives::{Address, address};

const MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Devnet Test Safes
pub const TEST_SAFES: [Address; 6] = [
    address!("cA070997F849985e99aF2Bab872009863cA94Ae1"),
    address!("835356Caa344aa7212F939CD3a951C6BB5e7bA05"),
    address!("53098E16a5b936021Aa320d364C823B24d7Fa00e"),
    address!("1435B7e344366a83AAaDD63523B085074C055643"),
    address!("93E9b3EF9BB350f1898Fe373b504129cE792830d"),
    address!("0995dd3e0dBAC55D2C3Fd360bAdC0b487fFAfA44"),
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
pub const WC_SEPOLIA_CHAIN_ID: u64 = 4801;
pub const DEV_CHAIN_ID: u64 = 2151908;

pub const PBH_NONCE_KEY: u64 = 0x7062687478;

pub const DEVNET_ENTRYPOINT: Address = address!("0000000071727De22E5E9d8BAf0edAc6f37da032");

pub const PBH_DEV_SIGNATURE_AGGREGATOR: Address =
    address!("Cf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9");

pub const PBH_DEV_ENTRYPOINT: Address = address!("9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");

pub const DEV_WORLD_ID: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");

pub mod bindings;
pub mod mock;
pub mod node;
pub mod pool;
pub mod utils;
