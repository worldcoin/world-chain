fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("nitro-worker requires Linux (AF_VSOCK)");
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    if let Err(e) = world_chain_nitro_worker::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
