# PBH Transactions

The World Chain Builder introduces the concept of PBH transactions, which are standard OP transactions that include a valid `PBHPayload` encoded in the tx calldata and target the `PBHEntryPoint`.
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
The `PBHEntryPoint` also allows 4337 bundle transactions to be included with top of block priority. A priority bundle is a bundle of UserOperations who's `sender` field is a PBH Safe, and who's `validationData` returned from `validateUserOp` contains an authorizer of the `PBHSignatureAggregator`.

```solidity
function validateUserOp(
        PackedUserOperation calldata userOp,
        bytes32,
        uint256 missingAccountFunds
) external onlySupportedEntryPoint returns (uint256 validationData) {}
```

Priority UserOperations append the abi.encoded [`PBHPayload`](https://github.com/worldcoin/world-chain/blob/efb6526be58493986b1f619ba4bffb76c13aa79d/contracts/src/interfaces/IPBHEntryPoint.sol#L14) on the `signature` field of the UserOperation, and bundlers are able to group priority UserOperations together based on the signature aggregator returned in the validation path when validating the UserOperation signature on the PBH Safe. 

The signature encoding scheme on a priority UserOperation leveraging the [PBH4337Module](../../contracts/src/PBH4337Module.sol) is as follows:

```js
    Bytes [0:12] Timestamp Validation Data
    Bytes [12: 65 * signatureThreshold + 12] ECDSA Signatures
    Bytes [65 * signatureThreshold + 12 : 65 * signatureThreshold + 364] ABI Encoded PBHPayload
```

After grouping priority UserOperation's together based on a unified `PBHSignatureAggregator` the bundler then interfaces with the `PBHSignatureAggregator` to strip off the encoded `PBHPayload` from the UserOperation signature, and accumulates an aggregated signature as an abi encoded vector of `PBHPayload`s.

```solidity
    /**
     * Aggregate multiple signatures into a single value.
     * This method is called off-chain to calculate the signature to pass with handleOps()
     * bundler MAY use optimized custom code perform this aggregation.
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
```

The aggregated signature accumulated from all UserOperation signatures is then checked against a few strict criteria in the `PBHEntryPoint` prior to proxying the `handleAggregatedOps` call to the singleton `EntryPoint`:

1.) The length of `PBHPayload`s on the aggregated signature must be equivelant to the amount of UserOperations in the bundle.

2.) The proofs must be valid on all `PBHPayload`s

3.) The `nullifierHash` on each `PBHPayload` must be unique

```solidity
    /// Execute a batch of PackedUserOperation with Aggregators
    /// @param opsPerAggregator - The operations to execute, grouped by aggregator (or address(0) for no-aggregator accounts).
    /// @param beneficiary      - The address to receive the fees.
    function handleAggregatedOps(
        IEntryPoint.UserOpsPerAggregator[] calldata opsPerAggregator,
        address payable beneficiary
    ) external virtual onlyProxy onlyInitialized nonReentrant {
        for (uint256 i = 0; i < opsPerAggregator.length; ++i) {

            // ------------snip---------------------

            PBHPayload[] memory pbhPayloads = abi.decode(opsPerAggregator[i].signature, (PBHPayload[]));
            require(
                pbhPayloads.length == opsPerAggregator[i].userOps.length,
                InvalidAggregatedSignature(pbhPayloads.length, opsPerAggregator[i].userOps.length)
            );
            for (uint256 j = 0; j < pbhPayloads.length; ++j) {
                address sender = opsPerAggregator[i].userOps[j].sender;
                // We now generate the signal hash from the sender, nonce, and calldata
                uint256 signalHash = abi.encodePacked(
                    sender, opsPerAggregator[i].userOps[j].nonce, opsPerAggregator[i].userOps[j].callData
                ).hashToField();

                _verifyPbh(signalHash, pbhPayloads[j]);
                nullifierHashes[pbhPayloads[j].nullifierHash] = true;
                emit PBH(sender, signalHash, pbhPayloads[j]);
            }
        }

        // ------------snip---------------------
    }
```

If all these conditions are met the bundle transaction will be proxied to the `EntryPoint` for execution of the UserOperations.

Note that the `PBHEntryPoint` hashes all UserOperations, and stores the hash in transient storage when executing a `handleAggregatedOps` call. The `PBHSignatureAggregator` then calls back into the `PBHEntryPoint` when called by the `EntryPoint` with `validateSignatures`. 

```solidity
function validateSignatures(PackedUserOperation[] calldata userOps, bytes calldata) external view {
        bytes memory encoded = abi.encode(userOps);
        pbhEntryPoint.validateSignaturesCallback(keccak256(encoded));
    }
```

This callback creates a cryptographic link between the `PBHSignatureAggregator` and the `PBHEntryPoint`, and prevents the bundler from modifying the destination of the transaction without causing the bundle to revert. This guarantees a Priority UserOperation will always be included in a priority bundle, and cannot be tampered with by the bundler. 

