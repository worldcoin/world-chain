# Major Changes
- Eth API should fetch the pending block directly from the BlockchainProvider

# validate_payload
- validate_block_with_state
   - trigger flashblocks stream over payloads built on parent hash
   - aggregate flashblocks into a `OpExecutionData`
   - pre-load EVM with bundle
- cleanup crate imports 

# Spec

I have given you the basic outline
