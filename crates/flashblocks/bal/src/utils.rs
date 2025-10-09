use alloy_eip7928::AccountChanges;
use alloy_rlp::Decodable;
use flashblocks_primitives::primitives::FlashblockBlockAccessListWire;

/// Decode an RLP-encoded BlockAccessList (Vec<AccountChanges>)
///
/// # Arguments
/// * `rlp_bytes` - The RLP-encoded bytes
///
/// # Returns
/// * `Result<Vec<AccountChanges>, alloy_rlp::Error>` - The decoded BlockAccessList
///
/// # Example
/// ```ignore
/// let encoded = alloy_rlp::encode(&account_changes);
/// let decoded = decode_block_access_list(&encoded).unwrap();
/// ```
pub fn decode_block_access_list(
    rlp_bytes: &[u8],
) -> Result<Vec<AccountChanges>, alloy_rlp::Error> {
    let mut buf = rlp_bytes;
    Vec::<AccountChanges>::decode(&mut buf)
}

/// Decode an RLP-encoded FlashblockBlockAccessListWire
///
/// # Arguments
/// * `rlp_bytes` - The RLP-encoded bytes
///
/// # Returns
/// * `Result<FlashblockBlockAccessListWire, alloy_rlp::Error>` - The decoded FlashblockBlockAccessListWire
///
/// # Example
/// ```ignore
/// let encoded = alloy_rlp::encode(&fbal_wire);
/// let decoded = decode_flashblock_access_list_wire(&encoded).unwrap();
/// ```
pub fn decode_flashblock_access_list_wire(
    rlp_bytes: &[u8],
) -> Result<FlashblockBlockAccessListWire, alloy_rlp::Error> {
    let mut buf = rlp_bytes;
    FlashblockBlockAccessListWire::decode(&mut buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::Encodable;
    use flashblocks_primitives::primitives::FlashblockBlockAccessListWire;
    use alloy_primitives::B256;

    #[test]
    fn test_decode_flashblock_access_list_wire() {
        // Create a sample FlashblockBlockAccessListWire
        let original = FlashblockBlockAccessListWire {
            min_tx_index: 0,
            max_tx_index: 10,
            fbal_accumulator: B256::default(),
            accounts: vec![],
        };

        // Encode it
        let mut encoded = vec![];
        original.encode(&mut encoded);

        // Decode it back
        let decoded = decode_flashblock_access_list_wire(&encoded).expect("Failed to decode");

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_real_block_access_list() {
        use alloy_primitives::hex;

        // RLP-encoded BlockAccessList from user
        let hex_data = "f902acf89f9400000961ef480eb55e80d19ad83579a64c007002c0f884a00000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001a00000000000000000000000000000000000000000000000000000000000000002a00000000000000000000000000000000000000000000000000000000000000003c0c0c0f89f940000bbddc7ce488642fb579f8b00f3a590007251c0f884a00000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000001a00000000000000000000000000000000000000000000000000000000000000002a00000000000000000000000000000000000000000000000000000000000000003c0c0c0f862940000f90827f1c53a10cb7a02335b175320002935f847f845a00000000000000000000000000000000000000000000000000000000000000000e3e280a084e280903759b608a846549d4fd62ae2b817f9d023bce0aaf373b8af9bbd15ccc0c0c0c0f88394000f3df6d732807ef1319fb7b8bb8522d0beac02f847f845a0000000000000000000000000000000000000000000000000000000000000000ce3e280a0000000000000000000000000000000000000000000000000000000000000000ce1a0000000000000000000000000000000000000000000000000000000000000200bc0c0c0da941000000000000000000000000000000000001000c0c0c0c0c0da941000000000000000000000000000000000001100c0c0c0c0c0e0942adc25665018aa1fe0e6bc666dac8fc2697ff9bac0c0c6c50183011499c0c0e994a94f5374fce5edbc8e2a8697c15331677e6ebf0bc0c0cccb01893635c9adc5de9c6602c3c20101c0";

        let rlp_bytes = hex::decode(hex_data).expect("Invalid hex data");

        // Decode the BlockAccessList
        let decoded = decode_block_access_list(&rlp_bytes).expect("Failed to decode");

        // Verify it decoded successfully
        println!("Decoded BlockAccessList:");
        println!("  min_tx_index: {}", decoded.min_tx_index);
        println!("  max_tx_index: {}", decoded.max_tx_index);
        println!("  fbal_accumulator: {:?}", decoded.fbal_accumulator);
        println!("  accounts count: {}", decoded.accounts.len());

        for (i, account) in decoded.accounts.iter().enumerate() {
            println!("\nAccount {}:", i);
            println!("  address: {:?}", account.address);
            println!("  storage_changes: {}", account.storage_changes.len());
            println!("  storage_reads: {}", account.storage_reads.len());
            println!("  balance_changes: {}", account.balance_changes.len());
            println!("  nonce_changes: {}", account.nonce_changes.len());
            println!("  code_changes: {}", account.code_changes.len());
        }
    }
}
