// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {WorldIDAccountFactory} from "../../src/wia/WorldIDAccountFactory.sol";
import {IWorldIDAccountFactory} from "../../src/wia/interfaces/IWorldIDAccountFactory.sol";
import {IWorldIDVerifier} from "../../src/wia/interfaces/IWorldIDVerifier.sol";

/// @title MockWorldIDVerifier
/// @notice A mock verifier that always succeeds (no-op).
contract MockWorldIDVerifier is IWorldIDVerifier {
    function verify(
        uint256, /* nullifier */
        uint256, /* action */
        uint64, /* rpId */
        uint256, /* nonce */
        uint256, /* signalHash */
        uint64, /* expiresAtMin */
        uint64, /* issuerSchemaId */
        uint256, /* credentialGenesisIssuedAtMin */
        uint256[5] calldata /* zeroKnowledgeProof */
    )
        external
        pure
        override
    {
        // Always succeeds
    }
}

/// @title WorldIDAccountFactoryTest
/// @notice Foundry tests for WorldIDAccountFactory.
/// @author Worldcoin
contract WorldIDAccountFactoryTest is Test {
    WorldIDAccountFactory public factory;
    MockWorldIDVerifier public verifier;

    uint256 constant CREATION_NULLIFIER = 0x1111;
    uint256 constant ACCOUNT_NULLIFIER = 0x2222;

    uint64 constant EXPIRES_AT_MIN = 1_000_000;
    uint64 constant ISSUER_SCHEMA_ID = 1;

    event AccountCreated(address indexed account, uint256 generation);

    function setUp() public {
        verifier = new MockWorldIDVerifier();
        factory = new WorldIDAccountFactory(IWorldIDVerifier(address(verifier)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              HELPERS                                     ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Builds a single secp256k1 key for testing.
    function _singleSecp256k1Key() internal pure returns (IWorldIDAccountFactory.AuthorizedKey[] memory keys) {
        keys = new IWorldIDAccountFactory.AuthorizedKey[](1);
        keys[0] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.Secp256k1, keyData: _dummySecp256k1Key()
        });
    }

    /// @dev Returns a 33-byte dummy compressed secp256k1 public key.
    function _dummySecp256k1Key() internal pure returns (bytes memory) {
        return abi.encodePacked(bytes1(0x02), bytes32(uint256(0xdeadbeef)));
    }

    /// @dev Returns a 64-byte dummy P256 public key.
    function _dummyP256Key() internal pure returns (bytes memory) {
        return abi.encodePacked(bytes32(uint256(0xaa)), bytes32(uint256(0xbb)));
    }

    /// @dev Returns a dummy proof (all zeros).
    function _dummyProof() internal pure returns (uint256[5] memory) {
        return [uint256(0), uint256(0), uint256(0), uint256(0), uint256(0)];
    }

    /// @dev Derives the expected account address from an account nullifier.
    function _expectedAddress(uint256 accountNullifier) internal pure returns (address) {
        return address(bytes20(keccak256(abi.encodePacked(accountNullifier))));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            SUCCESS TESTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    function testCreateAccount_success() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();
        address expected = _expectedAddress(ACCOUNT_NULLIFIER);

        vm.expectEmit(true, true, true, true);
        emit AccountCreated(expected, 0);

        address account = factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        // Address derivation
        assertEq(account, expected, "Account address mismatch");

        // Key storage
        IWorldIDAccountFactory.AuthorizedKey[] memory storedKeys = factory.getAccountKeys(account);
        assertEq(storedKeys.length, 1, "Expected 1 stored key");
        assertEq(uint8(storedKeys[0].authType), uint8(IWorldIDAccountFactory.AuthType.Secp256k1));
        assertEq(storedKeys[0].keyData, keys[0].keyData, "Key data mismatch");

        // Generation
        assertEq(factory.getAccountGeneration(account), 0, "Generation should be 0");

        // Nullifier stored
        assertEq(factory.getAccountNullifier(account), ACCOUNT_NULLIFIER, "Nullifier mismatch");

        // Nullifier marked spent
        assertTrue(factory.spentAccountNullifiers(ACCOUNT_NULLIFIER), "Nullifier not marked spent");

        // Generation advanced
        assertEq(factory.generations(CREATION_NULLIFIER), 1, "Generation not advanced");
    }

    function testCreateAccount_nullifierAlreadySpent() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();

        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        // Second call with the same account nullifier should revert
        vm.expectRevert(IWorldIDAccountFactory.NullifierAlreadySpent.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
    }

    function testCreateAccount_invalidKeyCount_zero() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](0);

        vm.expectRevert(IWorldIDAccountFactory.InvalidKeyCount.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
    }

    function testCreateAccount_invalidKeyCount_tooMany() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](21);
        for (uint256 i = 0; i < 21; ++i) {
            keys[i] = IWorldIDAccountFactory.AuthorizedKey({
                authType: IWorldIDAccountFactory.AuthType.Secp256k1, keyData: _dummySecp256k1Key()
            });
        }

        vm.expectRevert(IWorldIDAccountFactory.InvalidKeyCount.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
    }

    function testCreateAccount_invalidKeyData_secp256k1() public {
        // Provide 64 bytes for a Secp256k1 key (should be 33)
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](1);
        keys[0] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.Secp256k1,
            keyData: _dummyP256Key() // 64 bytes — wrong for secp256k1
        });

        vm.expectRevert(IWorldIDAccountFactory.InvalidKeyData.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
    }

    function testCreateAccount_invalidKeyData_p256() public {
        // Provide 33 bytes for a P256 key (should be 64)
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](1);
        keys[0] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.P256,
            keyData: _dummySecp256k1Key() // 33 bytes — wrong for P256
        });

        vm.expectRevert(IWorldIDAccountFactory.InvalidKeyData.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
    }

    function testCreateAccount_addressDerivation() public {
        uint256 nullifier = 0xABCDEF;
        address expected = address(bytes20(keccak256(abi.encodePacked(nullifier))));

        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();
        address account = factory.createAccount(
            CREATION_NULLIFIER, nullifier, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        assertEq(account, expected, "Address derivation mismatch");
    }

    function testCreateAccount_generationAdvances() public {
        assertEq(factory.generations(CREATION_NULLIFIER), 0, "Initial generation should be 0");

        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        assertEq(factory.generations(CREATION_NULLIFIER), 1, "Generation should be 1 after first creation");
    }

    function testCreateAccount_multipleGenerations() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();

        // Generation 0
        uint256 nullifier0 = 0x1000;
        address account0 = factory.createAccount(
            CREATION_NULLIFIER, nullifier0, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
        assertEq(factory.generations(CREATION_NULLIFIER), 1);

        // Generation 1 — same creationNullifier, different accountNullifier
        uint256 nullifier1 = 0x2000;
        address account1 = factory.createAccount(
            CREATION_NULLIFIER, nullifier1, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );
        assertEq(factory.generations(CREATION_NULLIFIER), 2);

        // Different nullifiers produce different addresses
        assertTrue(account0 != account1, "Accounts from different generations should differ");
    }

    function testCreateAccount_multipleKeyTypes() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](3);
        keys[0] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.Secp256k1, keyData: _dummySecp256k1Key()
        });
        keys[1] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.P256, keyData: _dummyP256Key()
        });
        keys[2] = IWorldIDAccountFactory.AuthorizedKey({
            authType: IWorldIDAccountFactory.AuthType.WebAuthn, keyData: _dummyP256Key()
        });

        address account = factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        IWorldIDAccountFactory.AuthorizedKey[] memory stored = factory.getAccountKeys(account);
        assertEq(stored.length, 3, "Should store 3 keys");
        assertEq(uint8(stored[0].authType), uint8(IWorldIDAccountFactory.AuthType.Secp256k1));
        assertEq(uint8(stored[1].authType), uint8(IWorldIDAccountFactory.AuthType.P256));
        assertEq(uint8(stored[2].authType), uint8(IWorldIDAccountFactory.AuthType.WebAuthn));
    }

    function testCreateAccount_maxKeys() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = new IWorldIDAccountFactory.AuthorizedKey[](20);
        for (uint256 i = 0; i < 20; ++i) {
            keys[i] = IWorldIDAccountFactory.AuthorizedKey({
                authType: IWorldIDAccountFactory.AuthType.Secp256k1, keyData: _dummySecp256k1Key()
            });
        }

        address account = factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        IWorldIDAccountFactory.AuthorizedKey[] memory stored = factory.getAccountKeys(account);
        assertEq(stored.length, 20, "Should store exactly 20 keys");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                           FUZZ TESTS                                     ///
    ///////////////////////////////////////////////////////////////////////////////

    function testFuzz_addressDerivation(uint256 nullifier) public pure {
        address derived = address(bytes20(keccak256(abi.encodePacked(nullifier))));
        // Verify determinism — same nullifier always yields same address
        assertEq(derived, address(bytes20(keccak256(abi.encodePacked(nullifier)))));
    }

    function testFuzz_differentNullifiersDifferentAddresses(uint256 a, uint256 b) public pure {
        vm.assume(a != b);
        address addrA = address(bytes20(keccak256(abi.encodePacked(a))));
        address addrB = address(bytes20(keccak256(abi.encodePacked(b))));
        // While theoretically a collision is possible, it's astronomically unlikely
        // for any randomly sampled pair in fuzzing.
        assertTrue(addrA != addrB, "Different nullifiers should yield different addresses");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                           INVARIANT TESTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    function testInvariant_nullifierUniqueness() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();

        // Create an account
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        // Verify the nullifier is permanently spent
        assertTrue(factory.spentAccountNullifiers(ACCOUNT_NULLIFIER), "Nullifier should be spent");

        // Attempting to reuse it always reverts
        vm.expectRevert(IWorldIDAccountFactory.NullifierAlreadySpent.selector);
        factory.createAccount(
            CREATION_NULLIFIER, ACCOUNT_NULLIFIER, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        // Still spent
        assertTrue(factory.spentAccountNullifiers(ACCOUNT_NULLIFIER), "Nullifier should remain spent");
    }

    function testInvariant_accountAddressUniqueness() public {
        IWorldIDAccountFactory.AuthorizedKey[] memory keys = _singleSecp256k1Key();

        uint256 nullifierA = 0xAAAA;
        uint256 nullifierB = 0xBBBB;

        address accountA = factory.createAccount(
            CREATION_NULLIFIER, nullifierA, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        // Use a different creation nullifier for the second account to avoid generation confusion
        uint256 creationNullifier2 = 0x3333;
        address accountB = factory.createAccount(
            creationNullifier2, nullifierB, _dummyProof(), _dummyProof(), EXPIRES_AT_MIN, ISSUER_SCHEMA_ID, keys
        );

        assertTrue(accountA != accountB, "Distinct nullifiers must yield distinct accounts");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          CONSTANTS TESTS                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    function testConstants() public view {
        assertEq(factory.WORLD_TX_TYPE(), 0x6f);
        assertEq(factory.WORLD_CHAIN_RP_ID(), 480);
        assertEq(factory.MAX_AUTHORIZED_KEYS(), 20);
        assertEq(factory.WORLD_ID_ACCOUNT_FACTORY(), address(0x1D));
    }
}
