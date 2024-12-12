# PBH Validator

As mentioned previously, Stage 1 of 4337 PBH features a PBHSignatureAggregator and PBHValidator contract, allowing a user to include a World ID proof encoded in the UserOp signature. The signature aggregator will call the PBHValidator for each UserOp included in the bundle, verifying the associated proof.

The `PBHValidator` contract will extract the proof data from the signature, validate proof inputs and verify the proof. The `signal` for the proof will consist of the `userOpHash`. Upon successful verification of the proof, the `PBHValidator` will bump the PBH nonce for the `nullifierHash` associated with the proof. The PBH nonce is used to ensure that a given World ID user does not use more than `n` transactions a month.

If the UserOp successfully clears all of these checks, a `PBH` event will be emitted indicating to the builder that this UserOp is a valid PBH user operation. The builder will only consider a “PBH” bundle for priority inclusion if all UserOps in the bundle emit a PBH event, and `aggregator` is specified as the `PBHSignatureAggregator`.
