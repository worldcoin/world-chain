/// Enclave connection arguments shared between subcommands.
#[derive(Debug, clap::Args)]
pub struct CommonArgs {
    /// vsock CID of the running Nitro Enclave.
    #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
    pub enclave_cid: u32,

    /// vsock port the enclave listens on.
    #[arg(
        long,
        env = "ENCLAVE_PORT",
        default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT
    )]
    pub enclave_port: u32,
}
