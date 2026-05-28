use alloy_rpc_types_engine::PayloadId;

const OP_ENGINE_PAYLOAD_ID_V3: u8 = 0x03;
const OP_ENGINE_PAYLOAD_ID_V4: u8 = 0x04;

/// Temporary compatibility fix for op-reth v2.1.0, which derives OP payload IDs
/// with `EngineApiMessageVersion::default()` and currently defaults to V4.
pub fn force_op_payload_id_v3(id: PayloadId) -> PayloadId {
    payload_id_with_version(id, OP_ENGINE_PAYLOAD_ID_V3)
}

/// Converts the externally visible V3 payload ID back to the V4 ID currently used
/// by reth's payload service as the internal job lookup key.
pub fn op_reth_payload_id_v4_lookup(id: PayloadId) -> PayloadId {
    let bytes = payload_id_bytes(id);
    if bytes[0] == OP_ENGINE_PAYLOAD_ID_V3 {
        payload_id_with_version(id, OP_ENGINE_PAYLOAD_ID_V4)
    } else {
        id
    }
}

pub fn payload_id_with_version(id: PayloadId, version: u8) -> PayloadId {
    let mut bytes = payload_id_bytes(id);
    bytes[0] = version;
    PayloadId::new(bytes)
}

fn payload_id_bytes(id: PayloadId) -> [u8; 8] {
    id.0.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_first_byte_to_v3() {
        let id = PayloadId::new([0x04, 1, 2, 3, 4, 5, 6, 7]);

        assert_eq!(
            force_op_payload_id_v3(id),
            PayloadId::new([0x03, 1, 2, 3, 4, 5, 6, 7])
        );
    }

    #[test]
    fn maps_external_v3_id_to_internal_v4_lookup() {
        let id = PayloadId::new([0x03, 1, 2, 3, 4, 5, 6, 7]);

        assert_eq!(
            op_reth_payload_id_v4_lookup(id),
            PayloadId::new([0x04, 1, 2, 3, 4, 5, 6, 7])
        );
    }

    #[test]
    fn leaves_non_v3_lookup_ids_unchanged() {
        let id = PayloadId::new([0x09, 1, 2, 3, 4, 5, 6, 7]);

        assert_eq!(op_reth_payload_id_v4_lookup(id), id);
    }
}
