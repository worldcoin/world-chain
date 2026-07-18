# Linear Creation Prompt: Arc Privacy Network Prototype

Create the milestones and issues below in this Linear project:

https://linear.app/worldcoin/project/arc-privacy-network-zk-fault-proof-design-488665fd0ba4/overview

## Execution Rules

1. Inspect the project, its existing milestones, and its existing issues before writing anything.
2. Preserve the existing `Research Spike` milestone and completed research issues. Do not rename, move, close, or delete existing work.
3. Avoid duplicates. Match both exact titles and semantically equivalent existing issues. If an equivalent item exists, reuse it and report that it was reused.
4. Create all new issues in the `Protocol` team, assign them to this project and the specified milestone, and leave them in `Backlog` with no assignee.
5. Use High priority for M0 through M4 and Medium priority for M5 through M7.
6. Use the estimates below if the Protocol team supports the same point values. Otherwise leave estimates unset and report that they were omitted.
7. Do not create new labels unless `privacy` or `prototype` already exist. Reuse those labels when available.
8. Add explicit `blocked by` relations listed below. Do not infer additional blocking relationships when a related relation is sufficient.
9. Do not set target dates. Do not change project priority or status.
10. After creation, return a report grouped by milestone with links to every created or reused issue and a list of anything that could not be created.

## Common Issue Template

Every issue description must contain these headings:

- `Context`
- `Objective`
- `Acceptance Criteria`
- `Dependencies`
- `Out of Scope`

Every implementation issue must include focused unit or integration tests in its acceptance criteria. Each milestone ends with an automated E2E issue that runs against the native local World Chain devnet (`just devnet up`). E2E coverage is not a substitute for issue-level tests.

## Prototype Invariants

Include these invariants in M0.1 and link back to M0.1 from all protocol-facing issues:

- World Chain provides the only canonical ordering and data-availability log.
- The pEVM has its own persistent database but no private mempool, P2P network, or consensus.
- Only blocks containing privacy-relevant inputs create a private transition.
- Privacy-relevant inputs initially include successful encrypted inbox submissions and successful shield deposits.
- A privacy input in public block `N` requires exactly one private transition commitment in public block `N+1`.
- A block without privacy inputs creates no private transition and requires no commitment in the next block.
- The inbox returns only an acknowledgement. Public execution cannot observe private return values, logs, reverts, or variable private gas usage.
- The prototype uses a fixed 32-byte MSK and low fixed private gas parameters.
- The MSK derives separate transaction-encryption, state-root-encryption, and persistence-encryption keys.
- Contract commitments contain an encrypted private state root, not the plaintext root.
- The prototype runs outside a TEE. TEE execution, dealer/DKG setup, and ZK disputes are later milestones.

## Milestone M0: Prototype Design Freeze

Description: Freeze the prototype protocol profile and implementation boundaries before parallel development begins.

### M0.1 - Document the asynchronous privacy prototype protocol

Estimate: 3

Objective: Update the architecture to specify deterministic `N -> N+1` private commitments, sparse private transitions, public liveness behavior, and deferred production security features.

Acceptance Criteria:

- The document contains every Prototype Invariant above.
- It explicitly supersedes same-block commitment language for the prototype only.
- It defines behavior for missing, duplicate, or invalid `N+1` commitments.
- It documents that private execution is required before accepting the next block after privacy inputs.
- It identifies TEE execution, DKG, private contracts, and ZK disputes as out of scope for the first prototype.

Dependencies: None.

### M0.2 - Specify the private transaction and encrypted envelope

Estimate: 3

Objective: Define a versioned signed private transaction and authenticated encrypted envelope.

Acceptance Criteria:

- The private transaction defines chain ID, nonce, gas limit, destination, value, calldata, sender signature, and transaction hash.
- The envelope defines version, epoch/key ID, ciphertext, encryption metadata, size limits, and authenticated context.
- Replay protection and sender recovery are specified.
- Unknown versions and key IDs have deterministic rejection behavior.

Dependencies: Blocked by M0.1.

### M0.3 - Specify private transition and commitment formats

Estimate: 3

Objective: Define the private transition header and the public `N+1` commitment payload.

Acceptance Criteria:

- The format binds transition index, source L2 block number/hash, parent transition, input root, receipts root, encrypted state root, nonce, key epoch, and STF version.
- Failed private transactions are committed through the input and receipt roots even when the state root is unchanged.
- The commitment cannot be replayed for another source block or branch.
- Serialization and hashing rules are unambiguous and test-vector friendly.

Dependencies: Blocked by M0.1 and M0.2.

### M0.4 - Select and version the prototype cryptographic suites

Estimate: 3

Objective: Select supported Rust implementations for HPKE, HKDF, and AEAD while retaining Arc-style domain separation.

Acceptance Criteria:

- A 32-byte fixed development MSK is the only root secret.
- Separate derivations exist for the global transaction keypair, per-source-block state-root key, and per-emission persistence key.
- Derivation labels, salts, nonces, AAD, and algorithm versioning are documented.
- Dependency maturity and the cost of matching Arc's X-Wing suite are evaluated.
- Stable derivation and encryption test vectors are specified.

Dependencies: Blocked by M0.1.

### M0.5 - Select the persistent private-state backend

Estimate: 3

Objective: Decide how revm state, private metadata, state-root calculation, atomic commits, restart, and unwind will be persisted.

Acceptance Criteria:

- The decision compares a dedicated MDBX-backed state store with a fuller embedded reth provider.
- The selected backend supports accounts, storage, bytecode, receipts, state roots, atomic block commits, and unwind.
- The design separates the storage interface from its plaintext prototype backend so encrypted TEE persistence can replace it later.
- A minimal schema and crate ownership boundary are documented.

Dependencies: Blocked by M0.1 and M0.3.

## Milestone M1: Encrypted Inbox to Failed pEVM Execution

Description: Include an encrypted private transaction in local devnet, decrypt and execute it against empty persistent private state, and record the expected failure without affecting public execution.

### M1.1 - Implement MSK-derived prototype keys

Estimate: 3

Objective: Implement the fixed MSK and three domain-separated derivation paths.

Acceptance Criteria:

- The global transaction encryption keypair is deterministic for the fixed devnet MSK.
- Per-source-block state-root keys use the specified block context.
- Per-emission persistence keys use fresh emission context.
- All M0.4 test vectors pass.
- Development key material is clearly marked as non-production.

Dependencies: Blocked by M0.4.

### M1.2 - Implement the Privacy Inbox predeploy

Estimate: 5

Objective: Add the public protocol entry point for encrypted private transactions.

Acceptance Criteria:

- The inbox exposes the active encryption public key and key/epoch ID.
- Submission accepts bounded ciphertext and validates only public metadata.
- Submission returns a constant acknowledgement and exposes no private result.
- A successful submission emits or otherwise creates a canonical input record.
- Reverted submissions cannot become private inputs.
- Foundry tests cover success, malformed metadata, oversized input, and acknowledgement behavior.

Dependencies: Blocked by M0.2 and M1.1.

### M1.3 - Implement private transaction signing and encryption utilities

Estimate: 3

Objective: Build reusable test/client utilities that create, sign, encrypt, and submit private transactions.

Acceptance Criteria:

- Utilities create the M0.2 private transaction format.
- Sender recovery, chain ID, nonce, and transaction hash are deterministic.
- Ciphertext decrypts only under the matching MSK-derived secret key and authenticated context.
- Utilities can submit the outer transaction to a local World Chain RPC endpoint.

Dependencies: Blocked by M0.2 and M1.1.

### M1.4 - Create the persistent private database schema

Estimate: 5

Objective: Initialize an empty private state database independent from public World Chain state.

Acceptance Criteria:

- Empty genesis contains no funded private accounts.
- The database stores accounts, storage, bytecode, receipts, transitions, pending commitments, and the public scan cursor.
- The database uses a separate configurable data directory.
- Closing and reopening preserves the private head and state root.
- Schema tests cover empty initialization and restart.

Dependencies: Blocked by M0.5.

### M1.5 - Implement atomic private-state overlays

Estimate: 5

Objective: Execute a candidate private transition without partially mutating persistent state.

Acceptance Criteria:

- Reads resolve against persistent state plus the current overlay.
- A successful seal atomically commits state, receipts, transition metadata, and cursor.
- An execution or storage error discards the entire overlay.
- Tests demonstrate commit, rollback, and process restart behavior.

Dependencies: Blocked by M1.4.

### M1.6 - Implement the initial pEVM transaction executor

Estimate: 5

Objective: Execute decrypted signed transactions with revm against private state.

Acceptance Criteria:

- The executor uses the private chain ID and source public block number/timestamp.
- The prototype gas limit and gas price policy are fixed and documented.
- Signature, nonce, balance, destination, value, and intrinsic gas checks are deterministic.
- An unfunded transfer returns the expected insufficient-funds result without changing state.
- Unit tests cover invalid signature, wrong chain ID, wrong nonce, and insufficient funds.

Dependencies: Blocked by M0.2 and M1.5.

### M1.7 - Extract privacy inputs from canonical public blocks

Estimate: 5

Objective: Scan canonical World Chain blocks and build the ordered private input batch only when privacy inputs exist.

Acceptance Criteria:

- Every public block advances a lightweight scan cursor.
- Only successful inbox submissions and shield events are extracted.
- Inputs preserve public transaction and log order.
- Blocks without privacy inputs do not open the pEVM state or create a private transition.
- Root commitment transactions do not trigger private execution.
- Tests cover mixed, reverted, empty, and reordered public transactions.

Dependencies: Blocked by M0.3 and M1.2.

### M1.8 - Persist failed private receipts and transition metadata

Estimate: 3

Objective: Record that an invalid private transaction was processed even when its state root is unchanged.

Acceptance Criteria:

- The receipt records source block, inner hash, recovered sender, status, gas result, and failure category.
- Input and receipt commitments change even when the private state root does not.
- Restart does not process the same canonical input twice.
- Tests verify deterministic receipt and commitment hashes.

Dependencies: Blocked by M1.6 and M1.7.

### M1.9 - Add encrypted failing-transaction devnet E2E coverage

Estimate: 5

Objective: Prove the complete first vertical slice on the native local devnet.

Acceptance Criteria:

- The E2E test starts or targets `just devnet up`.
- It reads the inbox encryption key, submits an encrypted unfunded transfer, and observes the public `ACK`.
- The private executor detects, decrypts, authenticates, and rejects the private transaction.
- Public block production continues and public execution is unaffected.
- The test restarts the private service and verifies no duplicate processing.

Dependencies: Blocked by M1.3, M1.6, M1.7, and M1.8.

## Milestone M2: Deterministic N+1 Commitments and Validation

Description: A privacy transition derived from block `N` is encrypted, committed in exactly block `N+1`, and independently checked by prototype validators holding the fixed development MSK.

### M2.1 - Implement private transition sealing

Estimate: 5

Objective: Seal the ordered inputs, receipts, and post-state into the M0.3 transition format.

Acceptance Criteria:

- Transition index advances only for blocks with privacy inputs.
- Source block number/hash and parent transition are bound.
- Input root, receipts root, and state root are deterministic.
- A failed-only batch seals successfully with an unchanged state root.
- Tests cover multiple inputs, failed inputs, and branch-specific source hashes.

Dependencies: Blocked by M0.3, M1.5, and M1.8.

### M2.2 - Implement per-block private state-root encryption

Estimate: 3

Objective: Encrypt the sealed plaintext state root before it crosses the private execution boundary.

Acceptance Criteria:

- The key is derived from the MSK and source block `N`, not commitment block `N+1`.
- A fresh nonce is generated and carried with ciphertext and authentication tag.
- AAD binds algorithm version, chain, epoch, STF version, and source block context.
- Validators can deterministically re-encrypt their computed root using the transmitted nonce.
- Test vectors cover success and tampered context.

Dependencies: Blocked by M1.1 and M2.1.

### M2.3 - Implement the private transition commitment registry

Estimate: 5

Objective: Store the opaque encrypted root and transition metadata in the public system contract.

Acceptance Criteria:

- The registry accepts the M0.3 commitment format.
- Commitments are indexed by transition index and source block hash.
- Duplicate, skipped, and wrong-parent commitments are rejected.
- The plaintext private state root is never submitted publicly.
- Foundry tests cover valid and invalid commitment sequences.

Dependencies: Blocked by M0.3 and M2.2.

### M2.4 - Inject the private transition commitment into N+1

Estimate: 5

Objective: Add a canonical builder-generated system transaction committing block `N` in block `N+1`.

Acceptance Criteria:

- A privacy batch in `N` produces exactly one commitment transaction in `N+1`.
- No privacy batch in `N` produces no commitment in `N+1`.
- Commitment placement relative to other builder/system transactions is deterministic.
- Inputs included in `N+1` remain a separate batch whose commitment is due in `N+2`.
- Builder tests cover consecutive private batches and empty gaps.

Dependencies: Blocked by M2.2 and M2.3.

### M2.5 - Implement validator-side private transition replay

Estimate: 5

Objective: Let a prototype validator derive the expected transition from privacy inputs in block `N`.

Acceptance Criteria:

- Validators use the same fixed development MSK and STF version.
- Validation extracts and executes the exact canonical input order.
- The expected transition is persisted until block `N+1` validation.
- Multiple validators derive identical transition and encrypted-root values.
- Tests compare builder and validator results for the same source block.

Dependencies: Blocked by M1.6, M1.7, M2.1, and M2.2.

### M2.6 - Validate required commitments in N+1

Estimate: 5

Objective: Enforce the deterministic relationship between privacy inputs in `N` and the commitment in `N+1`.

Acceptance Criteria:

- Missing, duplicate, malformed, wrong-source, wrong-parent, wrong-input, wrong-receipt, or wrong-root commitments are rejected.
- Unexpected commitments after a block with no privacy inputs are rejected.
- A valid commitment promotes the pending private transition.
- Tests exercise every rejection category.

Dependencies: Blocked by M2.4 and M2.5.

### M2.7 - Gate privacy transaction inclusion on executor readiness

Estimate: 3

Objective: Preserve public liveness by excluding new privacy transactions when the executor cannot guarantee the required next-block commitment.

Acceptance Criteria:

- Executor readiness is observable by the builder without exposing private state.
- An unavailable executor prevents new inbox transactions from being selected while ordinary public transactions continue.
- Once privacy inputs are included in `N`, the commitment requirement for `N+1` cannot be silently abandoned.
- Prototype crash-after-inclusion behavior is documented as an error requiring recovery before `N+1`.
- Tests cover ready, unavailable, and readiness-loss states.

Dependencies: Blocked by M1.7 and M2.4.

### M2.8 - Implement private restart, pending-transition recovery, and reorg unwind

Estimate: 5

Objective: Recover the scan cursor, private head, and pending `N+1` commitment after restart or canonical reorg.

Acceptance Criteria:

- Restart restores sealed and pending transition state.
- A reorg unwinds to the last transition whose source block remains canonical.
- Replacement-branch inputs are replayed in order.
- A commitment from an orphaned branch cannot validate on the replacement branch.
- Tests cover restart between `N` and `N+1` and a source-block reorg.

Dependencies: Blocked by M1.5, M2.1, and M2.5.

### M2.9 - Add deterministic commitment and validator devnet E2E coverage

Estimate: 5

Objective: Verify the `N -> N+1` protocol with builder and validator nodes on local devnet.

Acceptance Criteria:

- A private input in `N` produces one valid commitment in `N+1`.
- A block without private inputs creates no private transition or commitment.
- Consecutive private blocks produce commitments in consecutive following blocks.
- A test mutation of the commitment is rejected by the validator.
- Public-only blocks continue when privacy inclusion is disabled before accepting a private input.

Dependencies: Blocked by M2.6, M2.7, and M2.8.

## Milestone M3: Shield Bridge and Private Transfers

Description: Lock public devnet funds, mint the corresponding private balance, and execute a successful private value transfer.

### M3.1 - Implement prototype public shield escrow

Estimate: 5

Objective: Lock one selected devnet asset and emit a deterministic private deposit input.

Acceptance Criteria:

- The selected asset and custody semantics are documented.
- A successful shield locks funds and emits recipient, amount, asset, and unique deposit identity.
- Reverted or zero-value shields cannot mint private funds.
- Contract tests cover custody, event data, and failure cases.

Dependencies: Blocked by M1.2 and M2.3.

### M3.2 - Apply shield deposits to private state

Estimate: 5

Objective: Convert successful public shield events into protocol-level private balance credits.

Acceptance Criteria:

- Deposit identity binds source block hash, transaction index, and log index.
- Deposits execute in public order relative to encrypted transactions.
- Each deposit can mint exactly once across restart and replay.
- A shield followed by a private spend in the same source block follows deterministic ordering.
- Tests cover duplicate deposits, reorgs, and mixed input order.

Dependencies: Blocked by M1.7, M2.1, and M3.1.

### M3.3 - Implement successful private value transfers

Estimate: 5

Objective: Transfer private native value between funded private accounts.

Acceptance Criteria:

- Valid signature, chain ID, nonce, balance, and fixed gas rules are enforced.
- Sender and recipient balances and sender nonce update atomically.
- Private receipts contain no public return data.
- Tests cover success, replay, insufficient post-shield balance, and nonce ordering.

Dependencies: Blocked by M1.6 and M3.2.

### M3.4 - Add development-only private inspection RPC

Estimate: 3

Objective: Provide sufficient local observability to verify the non-TEE prototype.

Acceptance Criteria:

- RPC exposes private head, development balance lookup, transaction receipt, and transition metadata.
- Responses identify source public block and private transition index.
- The API is explicitly marked unsafe/development-only and is disabled by default outside local devnet.
- RPC tests cover known and unknown accounts and transactions.

Dependencies: Blocked by M1.4 and M2.1.

### M3.5 - Add shield and private-transfer devnet E2E coverage

Estimate: 5

Objective: Demonstrate the first successful private asset flow.

Acceptance Criteria:

- Alice's initial unfunded encrypted transfer fails.
- Alice shields public funds and receives the matching private balance.
- Alice submits an encrypted private transfer to Bob and it succeeds.
- Balances and nonce persist across restart.
- Each source block receives its required commitment in exactly the next block.

Dependencies: Blocked by M3.2, M3.3, and M3.4.

## Milestone M4: Unshield Bridge and Complete Asset Flow

Description: Burn private balance and release the corresponding public escrowed funds through a trusted prototype settlement path.

### M4.1 - Specify private withdrawal outputs and nullifiers

Estimate: 3

Objective: Define the private burn output consumed by public settlement.

Acceptance Criteria:

- The format binds asset, amount, public recipient, source transition, unique nullifier, and STF version.
- Withdrawal identity and replay protection are deterministic.
- Eligibility requires the source transition commitment to exist publicly.

Dependencies: Blocked by M0.3 and M3.3.

### M4.2 - Implement private unshield burn and withdrawal persistence

Estimate: 5

Objective: Burn or lock private balance and create a persistent withdrawal output.

Acceptance Criteria:

- Insufficient balance and duplicate requests fail without mutation.
- A valid request reduces private balance and creates one withdrawal output.
- Withdrawal output is bound into receipts and the resulting private transition.
- Tests cover success, insufficient balance, duplicate nonce, and restart.

Dependencies: Blocked by M4.1.

### M4.3 - Implement trusted prototype withdrawal finalization

Estimate: 5

Objective: Release public escrow using a privileged prototype publisher pending future proof verification.

Acceptance Criteria:

- Only the configured prototype publisher can finalize.
- Settlement requires the exact source private transition commitment.
- Each nullifier can release funds once.
- The contract cannot release more of an asset than it holds.
- The trusted assumption and future replacement by attestation/proof verification are explicit.
- Foundry tests cover authorization and double-finalization.

Dependencies: Blocked by M3.1, M4.1, and M4.2.

### M4.4 - Add complete shield-transfer-unshield devnet E2E coverage

Estimate: 5

Objective: Demonstrate the complete trusted prototype asset lifecycle.

Acceptance Criteria:

- Alice shields, privately transfers to Bob, and Bob requests an unshield.
- The source transition is committed in exactly the following public block.
- Trusted finalization releases the correct public amount to Bob.
- Duplicate finalization fails.
- Escrow and private balances reconcile before and after the flow.

Dependencies: Blocked by M3.5 and M4.3.

## Milestone M5: Private Contract Deployment and Execution

Description: Extend the pEVM from private value transfers to standard EVM bytecode deployment and contract calls.

### M5.1 - Implement private contract creation

Estimate: 5

Objective: Support private `CREATE` transactions using the existing encrypted inbox.

Acceptance Criteria:

- Contract address, creator nonce, bytecode, constructor execution, and receipt are deterministic.
- Failed deployments do not partially persist code or storage.
- Tests cover successful and reverted creation.

Dependencies: Blocked by M3.3.

### M5.2 - Persist private bytecode and contract storage

Estimate: 5

Objective: Support revm code and storage reads/writes across private transitions and restarts.

Acceptance Criteria:

- Code hash, bytecode, account metadata, and storage slots persist correctly.
- State-root calculation includes code and storage changes.
- Restart and unwind tests cover contract state.

Dependencies: Blocked by M1.4 and M5.1.

### M5.3 - Execute encrypted private contract calls

Estimate: 5

Objective: Execute normal EVM calls against private contracts without exposing public results.

Acceptance Criteria:

- Calldata, return data, logs, revert reason, and storage remain private.
- The public inbox continues to return only `ACK`.
- Private receipts capture success or failure for authorized development queries.
- Tests cover successful call, revert, storage mutation, and gas exhaustion.

Dependencies: Blocked by M5.2.

### M5.4 - Add private contract devnet E2E coverage

Estimate: 5

Objective: Deploy and call a simple private contract through encrypted transactions.

Acceptance Criteria:

- A contract is privately deployed, called, and queried through development RPC.
- Public block data contains ciphertext but no plaintext calldata, result, or private logs.
- State persists across restart and commitments remain valid.

Dependencies: Blocked by M5.3.

## Milestone M6: TEE Execution, Persistence, and Key Lifecycle

Description: Move decryption, private execution, root calculation, and persistence keys into an attested enclave while treating the host as untrusted.

### M6.1 - Define the host-to-enclave private execution protocol

Estimate: 5

Objective: Specify the minimal APIs for canonical inputs, execution, sealing, persistence, recovery, and health.

Acceptance Criteria:

- The host cannot request arbitrary state reads, state overrides, or unrestricted simulation.
- Requests bind chain, epoch, source block, parent transition, and STF version.
- Responses bind transition commitment, encrypted persistence artifacts, and enclave signing identity.

Dependencies: Blocked by M2.1 and M3.5.

### M6.2 - Encrypt snapshots and write-ahead logs

Estimate: 5

Objective: Persist private state through an untrusted host using MSK-derived per-emission keys.

Acceptance Criteria:

- State is plaintext only inside the enclave boundary.
- Every snapshot/WAL emission uses domain-separated key derivation and authenticated encryption.
- Tampering, truncation, fabrication, and wrong-epoch restoration fail authentication.
- Recovery verifies that restored state opens to a committed private root.

Dependencies: Blocked by M1.1, M1.5, and M6.1.

### M6.3 - Implement the dealer enclave and share generation

Estimate: 5

Objective: Generate epoch MSK material in an attested dealer and create threshold custody shares.

Acceptance Criteria:

- MSK generation, Shamir parameters, share wrapping, attestation binding, and dealer destruction behavior are specified and tested.
- Plaintext shares and MSK are never written to host storage.
- Development and production ceremony modes are clearly separated.

Dependencies: Blocked by M0.4.

### M6.4 - Implement seed recovery and attested MSK provisioning

Estimate: 5

Objective: Reconstruct MSK only inside an approved seed enclave and deliver it only to approved runtime enclaves.

Acceptance Criteria:

- Threshold shares reconstruct the correct epoch MSK.
- Recipient measurement, role, operator, epoch, and STF version are policy checked.
- Provisioning uses an attested encrypted channel.
- Rejected measurements never receive key material.

Dependencies: Blocked by M6.3.

### M6.5 - Run the pEVM and database boundary inside Nitro

Estimate: 5

Objective: Replace the plaintext prototype executor with the enclave protocol while preserving the private STF.

Acceptance Criteria:

- Transaction decryption, revm execution, state-root calculation, and persistence encryption occur inside Nitro.
- The host sees only public ciphertext, encrypted persistence artifacts, and protocol outputs.
- Existing prototype transition vectors match the enclave implementation.

Dependencies: Blocked by M6.1, M6.2, and M6.4.

### M6.6 - Add TEE-backed private execution devnet E2E coverage

Estimate: 5

Objective: Run the shield-transfer flow with Nitro-backed execution and encrypted persistence.

Acceptance Criteria:

- The E2E flow provisions the epoch key to an approved enclave.
- Shield and private transfer succeed with valid `N+1` commitments.
- Restart restores encrypted state without exposing plaintext to the host.
- A modified snapshot or unapproved enclave fails closed.

Dependencies: Blocked by M6.5.

### M6.7 - Design epoch rotation and DKG follow-up

Estimate: 3

Objective: Specify rotation, historical key retention, recovery, and replacement of dealer bootstrap with DKG.

Acceptance Criteria:

- Epoch boundaries bind transaction keys, root keys, persistence keys, STF version, and approved measurements.
- Historical replay and snapshot recovery requirements are documented.
- Dealer-to-DKG migration and runtime confidentiality limitations are explicit.

Dependencies: Blocked by M6.3 and M6.4.

## Milestone M7: Challenger and ZK Fault Proofs

Description: Add independent private replay, fault detection, disputes, and confidential proof-based adjudication.

### M7.1 - Implement independent canonical derivation for private validators

Estimate: 5

Objective: Use Kona or the selected OP derivation path to obtain canonical encrypted inputs independently of the sequencer execution service.

Acceptance Criteria:

- The derivation path reconstructs the exact public block and privacy input ordering.

- Reorg and safe/finalized ordering semantics are documented and tested.
- Derived input commitments match builder commitments.

Dependencies: Blocked by M2.6.

### M7.2 - Implement challenger enclave replay and fault detection

Estimate: 5

Objective: Independently execute private transitions and detect invalid commitments or attestations.

Acceptance Criteria:

- Challenger state remains independently persisted and synchronized.
- Mismatched root, input, receipt, epoch, STF version, or attestation is detected.
- Matching transitions require no public action.

Dependencies: Blocked by M6.5 and M7.1.

### M7.3 - Implement the private transition dispute lifecycle

Estimate: 5

Objective: Submit a challenge, freeze the disputed lineage, and prevent dependent unshields.

Acceptance Criteria:

- Challenges bind the exact source transition and claimed result.
- Submission and adjudication deadlines are separate.
- Dependent unshields freeze while public World Chain blocks remain canonical.
- Duplicate and stale challenges are rejected.

Dependencies: Blocked by M4.3 and M7.2.

### M7.4 - Implement confidential private-STF proof generation and verification

Estimate: 5

Objective: Resolve challenged transitions without revealing plaintext private state or transactions.

Acceptance Criteria:

- Public inputs bind pre-state, encrypted inputs, claimed post-state, chain context, epoch, and STF version.
- Sensitive state and plaintext transactions remain witness data.
- Valid proof, invalid proof, and proof-timeout outcomes are implemented.
- Settlement fails closed when the proposer cannot prove before the deadline.

Dependencies: Blocked by M7.3.

### M7.5 - Add invalid private-root challenge devnet E2E coverage

Estimate: 5

Objective: Demonstrate detection and adjudication of a deliberately incorrect private transition.

Acceptance Criteria:

- The test injects an invalid root or transition commitment.
- A challenger detects and disputes it.
- Dependent unshielding is frozen.
- Confidential proof adjudication rejects the invalid lineage or times out closed.
- The corrected private lineage can continue from the last valid transition.

Dependencies: Blocked by M7.4.

## Final Validation

After creating everything:

1. Verify every issue belongs to the target project and intended milestone.
2. Verify all M0-M4 issues are High priority and M5-M7 issues are Medium priority.
3. Verify all explicit blocking relations were created.
4. Verify no issue was accidentally assigned to a person.
5. Verify the existing Research Spike milestone and issues were untouched.
6. Return counts for milestones created, issues created, issues reused, and failures.
