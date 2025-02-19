use clap::Parser;
use semaphore::Field;

#[derive(Parser, Debug, Clone)]
pub struct Args {
    /// The number of PBH transactions to send to WC Sepolia Testnet
    #[clap(long, short, default_value_t = 1)]
    pub pbh_batch_size: u8,
    /// The number of Non-PBH transactions to send to WC Sepolia Testnet
    #[clap(long, short, default_value_t = 0)]
    pub tx_batch_size: u8,
    /// Identity commitment of the PBH transactions
    #[clap(long, short)]
    pub identity_commitment: Field,
    /// The private key signer for the transactions
    #[clap(long, short)]
    pub private_key: String,
    /// The RPC URL for WC Sepolia Testnet
    #[clap(long, short, required = true)]
    pub rpc_url: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

}