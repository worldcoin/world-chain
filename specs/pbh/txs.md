# PBH Transactions

The World Chain Builder introduces the concept of PBH transactions, which are standard OP transactions that target the [PBHEntryPoint](https://github.com/worldcoin/world-chain/blob/main/contracts/src/PBHEntryPointImplV1.sol) and includes a [PBHPayload](./payload.md) encoded in the tx calldata.
<!--TODO: uncomment this once the pbh sidecar is merged to main The World Chain Builder introduces the concept of PBH transactions, which are standard OP transactions that include a valid `PBHPayload` either encoded in the `WorldChainTxEnvelope` or in tx calldata and target the `PBHEntryPoint`. -->


<!--TODO: uncomment once the pbh sidecar is merged tom main ## World Chain Tx Envelope
The `WorldChainTxEnvelope` is an EIP-2718 transaction envelope that extends the standard `OpTxEnvelope`, optionally including a `PBHSidecar`. -->

## PBH Multicall
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
        uint256 signalHash = abi.encode(msg.sender, calls).hashToField();
        _verifyPbh(signalHash, pbhPayload);
        nullifierHashes[pbhPayload.nullifierHash] = true;

        returnData = IMulticall3(_multicall3).aggregate3(calls);
        emit PBH(msg.sender, signalHash, pbhPayload);
    }
```

This function takes an array of `calls` and a `PBHPayload`. During [transaction validation](./validation.md), the World Chain Builder will validate the payload and mark the transaction for priority inclusion. Visit the [validation](./validation.md#signal-hash) section of the docs to see how to encode the `signalHash` for a PBH Multicall.

## PBH 4337 UserOps
The `PBHEntryPoint` contract also provides priority inclusion for 4337 [UserOps](https://eips.ethereum.org/EIPS/eip-4337#useroperation) through PBH bundles. A PBH bundle is a standard 4337 bundle where the aggregated signature field is consists of an array of `PBHPayload`. A valid PBH bundle should include a `n` `PBHPayload`s, with each item corresponding to a `UserOp` in the bundle.


When creating a PBH `UserOp`, users will append the `PBHPayload` to the [signature](https://github.com/eth-infinitism/account-abstraction/blob/ed8a5c79b50361b2f1742ee9efecd45f494df597/contracts/interfaces/PackedUserOperation.sol#L27) field and specify the [PBHSignatureAggregator]() as the [sigAuthorizer](https://github.com/eth-infinitism/account-abstraction/blob/ed8a5c79b50361b2f1742ee9efecd45f494df597/contracts/legacy/v06/IAccount06.sol#L25-L26). The `UserOp` can then be sent to a 4337 bundler that supports PBH and maintains an alt-mempool for PBH `UserOps`. 

The bundler will [validate the PBHPayload](./validation.md), strip the payload from the `userOp.signature` field and add it to the aggregated signature. 

```solidity
    /**
     * Aggregate multiple signatures into a single value.
     * This method is called off-chain to calculate the signature to pass with handleOps()
     * @param userOps              - Array of UserOperations to collect the signatures from.
     * @return aggregatedSignature - The aggregated signature.
     */
    function aggregateSignatures(PackedUserOperation[] calldata userOps)
        external
        view
        returns (bytes memory aggregatedSignature)
    {
        IPBHEntryPoint.PBHPayload[] memory pbhPayloads = new IPBHEntryPoint.PBHPayload[](userOps.length);
        for (uint256 i = 0; i < userOps.length; ++i) {
            (, bytes memory proofData) = SafeModuleSignatures.extractProof(
                userOps[i].signature, ISafe(payable(userOps[i].sender)).getThreshold()
            );
            pbhPayloads[i] = abi.decode(proofData, (IPBHEntryPoint.PBHPayload));
        }
        aggregatedSignature = abi.encode(pbhPayloads);
    }
}
```

Upon submitting a PBH bundle to the network, the World Chain builder will ensure that all PBH bundles have valid proofs and mark the bundle for priority inclusion.

Visit the [validation](./validation.md#signal-hash) section of the docs to see how to encode the `signalHash` for a PBH `UserOps` work, check out the [handleAggregatedOps()](https://github.com/worldcoin/world-chain/blob/main/contracts/src/PBHEntryPointImplV1.sol#L216-L250) function and [PBH4337Module](https://github.com/worldcoin/world-chain/blob/main/contracts/src/PBH4337Module.sol).
