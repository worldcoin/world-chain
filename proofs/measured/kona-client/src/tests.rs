mod output_version {
    use alloy_primitives::{B256, keccak256};
    use kona_preimage::PreimageKey;
    use kona_proof::{block_on, errors::OracleProviderError, sync::fetch_safe_head_hash};
    use world_chain_proof_core::witness::preimage_store::PreimageStore;

    fn oracle_for_output(preimage: [u8; 128]) -> (PreimageStore, B256) {
        let root = keccak256(preimage);
        let mut oracle = PreimageStore::default();
        oracle
            .save_preimage(PreimageKey::new_keccak256(root.0), preimage.to_vec())
            .unwrap();
        (oracle, root)
    }

    #[test]
    fn version_zero_output_returns_safe_head() {
        let safe_head = B256::repeat_byte(0x42);
        let mut preimage = [0u8; 128];
        preimage[96..].copy_from_slice(safe_head.as_slice());
        let (oracle, root) = oracle_for_output(preimage);

        assert_eq!(
            block_on(fetch_safe_head_hash(&oracle, root)).unwrap(),
            safe_head
        );
    }

    #[test]
    fn nonzero_output_version_is_rejected() {
        for index in [0, 31] {
            let mut preimage = [0u8; 128];
            preimage[index] = 1;
            let version = B256::from_slice(&preimage[..32]);
            let (oracle, root) = oracle_for_output(preimage);

            let err = block_on(fetch_safe_head_hash(&oracle, root))
                .expect_err("unsupported output version must be rejected");
            assert!(matches!(err, OracleProviderError::UnknownOutputVersion(v) if v == version));
        }
    }
}

mod kzg {
    use alloy_primitives::hex_literal::hex;
    use kona_sp1_client_utils::precompiles::CustomCrypto;
    use kzg_rs::{Bytes32, Bytes48, KzgProof, KzgSettings};
    use revm::precompile::{Crypto, PrecompileHalt};

    #[test]
    fn invalid_kzg_opening_is_rejected() {
        let commitment = hex!(
            "8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7"
        );
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let mut y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9");
        let proof = hex!(
            "a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c"
        );

        // Keep the input well-formed, but make the claimed evaluation wrong.
        y[31] ^= 1;

        let crypto = CustomCrypto::default();
        let raw_result = KzgProof::verify_kzg_proof(
            &Bytes48::from_slice(&commitment).unwrap(),
            &Bytes32::from_slice(&z).unwrap(),
            &Bytes32::from_slice(&y).unwrap(),
            &Bytes48::from_slice(&proof).unwrap(),
            &KzgSettings::load_trusted_setup_file().unwrap(),
        );
        assert!(
            matches!(raw_result, Ok(false)),
            "raw kzg-rs result: {raw_result:?}"
        );

        assert!(matches!(
            crypto.verify_kzg_proof(&z, &y, &commitment, &proof),
            Err(PrecompileHalt::BlobVerifyKzgProofFailed)
        ));
    }

    #[test]
    fn valid_kzg_opening_is_accepted() {
        let commitment = hex!(
            "8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7"
        );
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9");
        let proof = hex!(
            "a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c"
        );

        let crypto = CustomCrypto::default();
        assert_eq!(crypto.verify_kzg_proof(&z, &y, &commitment, &proof), Ok(()));
    }
}
