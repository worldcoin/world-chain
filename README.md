# world-chain-builder

## Priority Blockspace for Humans (PBH)

### Intro

The general idea behind PBH transaction is to allow orb verified users to submit a limited number of priority transactions.
Priority transactions are alloted a fixed amount of the total available block space, and are executed at the top of the block before other non verified transactions.
This is useful as it provides verified users a way to submit transactions without competing with bots on fees or being exposed to potentially harmful MEV activity.

### Implementation

Pbh transactions are composed of any regular rlp encoded optimism transaction followed by an rlp encoded PBH payload.
The PBH payload contains the following fields:

`external_nullifier` - A string containing a prefix, the date marker, and the pbh nonce

`external_nullifier_hash` - The hash of the external nullifier

`nullifier_hash` - A nullifier hash used to keep track of previously used pbh transactions

`signal_hash` - This is the transaction hash which associates this proof with a specific transaction

`root` - The root of the merkle tree for which this proof was generated

`proof` - The actual semaphore proof verifying that the sender is included in the set of orb verified users
