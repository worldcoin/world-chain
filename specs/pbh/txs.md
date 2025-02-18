# PBH Transactions

The World Chain Builder introduces the concept of PBH transactions, which are standard OP transactions that include a valid `PBHPayload` encoded in the tx calldata and target the `PBHEntryPoint`.
<!--TODO: uncomment this once the pbh sidecar is merged to main The World Chain Builder introduces the concept of PBH transactions, which are standard OP transactions that include a valid `PBHPayload` either encoded in the `WorldChainTxEnvelope` or in tx calldata and target the `PBHEntryPoint`. -->


## PBHPayload
A `PBHPayload` consists of the following RLP encoded fields:

| Field                 | Type        | Description |
|-----------------------|------------|-------------|
| `external_nullifier`  | `bytes`    | A unique identifier derived from the proof version, date marker, and nonce. |
| `nullifier_hash`      | `Field`    | A cryptographic nullifier ensuring uniqueness and preventing double-signaling. |
| `root`               | `Field`    | The Merkle root proving inclusion in the World ID set. |
| `proof`              | `Proof`    | Semaphore proof verifying membership in the World ID set. |

### External Nullifier

The `external_nullifier` is a unique identifier used when verifying PBH transactions to prevent double-signaling and enforce PBH transaction rate limiting. For a given `external_nullifier`, the resulting [nullifier_hash](https://docs.semaphore.pse.dev/glossary#nullifier) will be the same, meaning it can be used a unique marker for a given World ID user while maintaining all privacy preserving guarantees provided by World ID. Note that since the `external_nullifier` is different for every PBH transaction a user sends, no third party can know if two PBH transactions are from the same World ID. 

The `external_nullifier` is used to enforce that a World ID user can only submit `n` PBH transactions per month, with the Builder ensuring that all proofs and `external_nullifiers` are valid before a transaction is included.


The `external_nullifier` is encoded as a 32-bit packed unsigned integer, with the following structure:
- **Version (`uint8`)**: Defines the schema version.
- **Year (`uint16`)**: The current year expressed as `yyyy`.
- **Month (`uint8`)**: The current month expressed as (1-12).
- **PBH Nonce (`uint8`)**: The PBH nonce, where `0 <= n < pbhNonceLimit`.



Below is an example of how to encode and decode the `external_nullifier` for a given PBH transaction.

```solidity
uint8 public constant V1 = 1;

/// @notice Encodes a PBH external nullifier using the provided year, month, and nonce.
/// @param version An 8-bit version number (0-255) used to identify the encoding format.
/// @param pbhNonce An 8-bit nonce value (0-255) used to uniquely identify the nullifier within a month.
/// @param month An 8-bit 1-indexed value representing the month (1-12).
/// @param year A 16-bit value representing the year (e.g., 2024).
/// @return The encoded PBHExternalNullifier.
function encode(uint8 version, uint8 pbhNonce, uint8 month, uint16 year) internal pure returns (uint256) {
    require(month > 0 && month < 13, InvalidExternalNullifierMonth());
    return (uint256(year) << 24) | (uint256(month) << 16) | (uint256(pbhNonce) << 8) | uint256(version);
}

/// @notice Decodes an encoded PBHExternalNullifier into its constituent components.
/// @param externalNullifier The encoded external nullifier to decode.
/// @return version The 8-bit version extracted from the external nullifier.
/// @return pbhNonce The 8-bit nonce extracted from the external nullifier.
/// @return month The 8-bit month extracted from the external nullifier.
/// @return year The 16-bit year extracted from the external nullifier.
function decode(uint256 externalNullifier)
    internal
    pure
    returns (uint8 version, uint8 pbhNonce, uint8 month, uint16 year)
{
    year = uint16(externalNullifier >> 24);
    month = uint8((externalNullifier >> 16) & 0xFF);
    pbhNonce = uint8((externalNullifier >> 8) & 0xFF);
    version = uint8(externalNullifier & 0xFF);
}
```
<br>

The **World Chain Builder** enforces:
- The `external_nullifier` is correctly formatted and includes the current year, month and valid nonce.
- The `proof` is valid and specifies the `external_nullifier` as a public input to the proof.
- The `external_nullifier` has not been used before, ensuring that the `pbh_nonce` is unique for the given month and year.



### Signal Hash

One of the inputs when [generating a World ID proof](https://docs.rs/semaphore-rs/0.3.1/semaphore_rs/protocol/fn.generate_proof.html) is the `signal_hash`. 
As the name suggests, the `signal_hash` is the hash of the [signal](https://docs.semaphore.pse.dev/V2/technical-reference/circuits#signal) which is an arbitrary message provided by the prover, allowing the verifier to hash the `signal` when verifying the proof, ensuring that the proof was generated with the expected inputs.

Within the context of PBH, the `signal_hash` is used to ensure the proof was created for the transaction being submitted. Depending on the type of the PBH transaction, this value could be a hash of the tx calldata, a 4337 UserOp hash or the tx hash itself.

<!--TODO: uncomment once the pbh sidecar is merged tom main ## World Chain Tx Envelope
The `WorldChainTxEnvelope` is an EIP-2718 transaction envelope that extends the standard `OpTxEnvelope`, optionally including a `PBHSidecar`. -->


## `PBHEntryPoint`
To submit a valid PBH transaction, users can call the `pbhMulticall()` function on the `PBHEntryPoint` contract.


```solidity
    /// @notice Executes a Priority Multicall3.
    /// @param calls The calls to execute.
    /// @param pbhPayload The PBH payload containing the proof data.
    /// @return returnData The results of the calls.
    function pbhMulticall(IMulticall3.Call3[] calldata calls, PBHPayload calldata pbhPayload)
        external
        virtual
        onlyProxy
        onlyInitialized
        nonReentrant
        returns (IMulticall3.Result[] memory returnData)
    {
        if (gasleft() > pbhGasLimit) {
            revert GasLimitExceeded(gasleft(), pbhGasLimit);
        }

        uint256 signalHash = abi.encode(msg.sender, calls).hashToField();
        _verifyPbh(signalHash, pbhPayload);
        nullifierHashes[pbhPayload.nullifierHash] = true;

        returnData = IMulticall3(_multicall3).aggregate3(calls);
        emit PBH(msg.sender, signalHash, pbhPayload);
    }
```

This function takes an array of `calls` and a [PBHPayload](https://github.com/worldcoin/world-chain/blob/main/contracts/src/interfaces/IPBHEntryPoint.sol#L14-L19) where the `signalHash` is specified as `uint256 signalHash = abi.encode(msg.sender, calls).hashToField();`.
During transaction validation, the World Chain Builder will validate the `PBHPayload` and mark the transaction for priority inclusion.

## PBH 4337 UserOps
  - note that if specifying the pbh signature aggregator, the bundler will send via pbhentrypoint, if no signature aggregator is specified, then the bundler will submit the bundle with a WorldChainTxEnvelope.

