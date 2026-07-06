#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nitro-worker requires Linux (AF_VSOCK)");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    if let Err(e) = world_chain_nitro_worker::run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
