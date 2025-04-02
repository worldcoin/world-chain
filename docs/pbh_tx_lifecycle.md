# Lifecycle of a PBH Transaction

The World Chain Builder is a custom block builder for the OP Stack that provides Priority Blockspace for Humans (PBH). PBH enables verified World ID users to execute transactions with top of block priority, enabling a more frictionless user experience. This mechanism is designed to ensure that ordinary users aren’t unfairly disadvantaged by automated systems and greatly mitigates the impact of negative impacts of MEV. PBH also enables future flexibility, allowing for a separate EIP-1559-style fee market mechanism for verified transactions.

The builder introduces a new [EIP-2718 RLP encoded transaction envelope](https://eips.ethereum.org/EIPS/eip-2718) including the necessary data to verify the transaction was created by a valid World ID user. To get a deeper understanding of PBH, lets walk through the life cycle of a transaction. 

</br>

## Creating a PBH transaction

The contents of the PBH tx envelope simply consist of an [Ethereum typed transaction ](https://eips.ethereum.org/EIPS/eip-2718) and optional semaphore proof ensuring that the sender is verified World ID user. In order to create a PBH transaction envelope, first generate an [Ethereum transaction](https://ethereum.org/en/developers/docs/transactions/).

Next, [create a World ID proof](https://docs.world.org/world-id/further-reading/zero-knowledge-proofs), **setting the `signal` to the transaction hash of the tx you are verifying**, and set the `externalNullifier` to the following schema `vv-mmyyyy-nn` where:

- **Version Prefix (vv)**: Indicates the version of the external nullifier schema This should be set to `0`.
- **Date (mmyyyy)**: Indicates the current month and year.
- **PBH Nonce (nn)**: A `u16` value used to rate-limit PBH transactions. 

Upon receiving the PBH transaction envelope, the World Chain Builder first validates the inner Ethereum transaction and then verifies the PBH payload. The builder enforces a transaction limit for each verified user that resets every month (eg. 50 txs per month), tracked by the PBH nonce specified in the `externalNullifier`. The user creating the PBH envelope must track which nonces they have used, however nonces can be specified in any order. For example, a user could send a PBH tx envelope with a PBH nonce of `16`, followed by a PBH nonce of `10` and so on. Additional transaction validation will be covered further in a later section. 

Below is a quick look at the `PbhTxEnvelope` in its entirety.

```
PbhTxEnvelope = { Tx, PBHPayload }
PBHPayload = { externalNullifier, nullifierHash, root, proof }
```
- `Tx`: Any valid Ethereum typed transaction.
- `externalNullifier`: String identifier used to ensure the uniqueness and proper sequencing of PBH transactions formatted as: `vv-mmyyyy-nn`.

- `nullifierHash`: Hash of the identity nullifier and the external nullifier; used to prevent double-signaling. You can read more [about the nullifier and external nullifier here](https://docs.world.org/world-id/further-reading/protocol-internals#external-nullifier).

- `root`: Root of the [Merkle tree representing the identity set](https://docs.world.org/world-id/further-reading/protocol-internals#signup-sequencer). This is the root used when creating the inclusion proof necessary to create a semaphore ZK proof.

- `proof`: The semaphore proof verifying that the sender is a member of the identity set.

</br>


## Sending transactions to the Builder

Since the PBH tx envelope is a valid [EIP-2718 Typed Transaction Envelope](https://eips.ethereum.org/EIPS/eip-2718), it can be sent to the builder via the `eth_sendRawTransaction` endpoint, just like any other node that implements the [Eth API](https://ethereum.org/en/developers/docs/apis/json-rpc/).

```bash
curl -X POST \
     -H "Content-Type: application/json" \
     -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_sendRawTransaction\",\"params\":[\"$PBH_TX_BYTES\"],\"id\":480}" \
     $BUILDER_ENDPOINT
```

Note that the builder is built on top of `op-reth` meaning that any valid transaction that can be sent to Optimism can also be sent to the builder. All transactions without a PBH payload attached are also forwarded to the sequencer.


</br>

## Transaction Validation

Once the World Chain Builder receives a new PBH tx envelope, it first verifies that the transaction attached is valid. Next, the PBH payload is verified, ensuring that the `externalNullifier` schema matches the expected version and that the PBH nonce does not exceed the maximum amount of transactions per period. 

Following this, the nullifier hash is checked to ensure that this user has not created a proof for this PBH nonce before. Finally, the ZK proof is verified and the builder ensures that the `signal` of the proof matches the transaction hash of the tx provided.

After successful validation, the transaction is inserted into the mempool.

</br>

## Transaction Priority and Block Production

When the sequencer sends a new `ForkChoiceUpdate` notifying that a new block should be produced, the builder will fetch the best transactions from the transaction pool to fill the block. The World Chain Builder tx pool implements a custom ordering policy which gives priority to transactions with a valid PBH payload. When the builder is including a PBH tx in the block, the inner transaction is unwrapped and the PBH payload is dropped. There is no additional data included in the sealed block and all World Chain blocks follow the same specification any other superchain compliant OP Stack chain. 

To ensure that there is always blockspace for non-verified transactions on World Chain, the builder enforces a `verified_blockspace_capacity` which specifies the maximum amount of gas that verified txs can occupy in a single block. This capacity is specified as a percentage of the block's gas limit. 

In the case where there are more verified transactions than can fit in the block, the remaining verified transactions will be included in the next block. Alternatively, when there are not enough verified transactions to fill this blockspace, non-verified transactions will be used to fill the remainder of the block. Default transaction ordering is used for all other transactions.
