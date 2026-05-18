use std::{borrow::Cow, env};

pub const WORLD_CHAIN_CLIENT_NAME: &str = "world-chain";
pub const WORLD_CHAIN_CLIENT_VERSION_SHA: &str = env!("VERGEN_GIT_SHA_SHORT");
pub const WORLD_CHAIN_CLIENT_VERSION: &str = env!("WORLD_CHAIN_CLIENT_VERSION");

use reth_node_core::version::{RethCliVersionConsts, try_init_version_metadata};

pub fn init_version_metadata() {
    try_init_version_metadata(version_metadata())
        .expect("version metadata should be generated in build.rs");
}

pub fn version_metadata() -> RethCliVersionConsts {
    RethCliVersionConsts {
        name_client: Cow::Borrowed("World Chain"),
        cargo_pkg_version: Cow::Borrowed(env!("CARGO_PKG_VERSION")),
        vergen_git_sha_long: Cow::Borrowed(env!("VERGEN_GIT_SHA")),
        vergen_git_sha: Cow::Borrowed(env!("VERGEN_GIT_SHA_SHORT")),
        vergen_build_timestamp: Cow::Borrowed(env!("VERGEN_BUILD_TIMESTAMP")),
        vergen_cargo_target_triple: Cow::Borrowed(env!("VERGEN_CARGO_TARGET_TRIPLE")),
        vergen_cargo_features: Cow::Borrowed(env!("VERGEN_CARGO_FEATURES")),
        short_version: Cow::Borrowed(env!("RETH_SHORT_VERSION")),
        long_version: Cow::Owned(format!(
            "{}\n{}\n{}\n{}\n{}",
            env!("RETH_LONG_VERSION_0"),
            env!("RETH_LONG_VERSION_1"),
            env!("RETH_LONG_VERSION_2"),
            env!("RETH_LONG_VERSION_3"),
            env!("RETH_LONG_VERSION_4"),
        )),
        build_profile_name: Cow::Borrowed(env!("RETH_BUILD_PROFILE")),
        p2p_client_version: Cow::Borrowed(env!("RETH_P2P_CLIENT_VERSION")),
        extra_data: Cow::Owned(extra_data()),
    }
}

fn extra_data() -> String {
    format!(
        "{WORLD_CHAIN_CLIENT_NAME}/{}/{}",
        env!("CARGO_PKG_VERSION"),
        env::consts::OS
    )
}
