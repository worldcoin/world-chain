# World Chain Builder

This is an implementation of a block builder for proposer / builder separation (PBS) in the optimism stack. Please see [this guide](https://github.com/flashbots/optimism/blob/daa43f158ffca0bfaba18391f688fed1d8a8f3d9/pbs/README.md) on how to run and test with a local devnet.

## Block Builder API Interation

The builder requires an [op-node](https://github.com/flashbots/optimism/tree/pbs) that publishes an event stream with the block payload attributes to trigger block building. 

To request a block, the proposer does a http request to the builder in parallel with its local node for a block as a failsafe. The following sequence diagram describes the interactions between the builder and the proposer.

```mermaid
sequenceDiagram
    participant OPS as Op-Node (Sequencer)
    participant ELS as Execution Client (Sequencer)
    participant OPB as Op-Node (Builder)
    participant ELB as Execution Client (Builder)

    %% Do we need this? Can the payload attributes just be sent with the fcu?
    ELB-->>OPB: Subscribe to `payload_attributes` events

    OPS-->>ELS: POST `engine_forkchoiceUpdated(forkchoiceState, PayloadAttributes)`
    ELS->>ELS: Start building block for `payloadId`

    OPS->>OPB: Peer `forkChoiceUpdated` notification
    OPB-->>ELB: POST `engine_forkchoiceUpdated(forkchoiceState, PayloadAttributes)`
    ELB->>ELB: Start building block for `payloadId`
    
    Note over OPS: Request a new block from the builder with corresponding `payloadId`
    OPS->> ELB: POST /eth/v1/builder/payload
    Note over OPS: Request a default block from sequencer execution client
    OPS->> ELS: engine_getPayload

    ELB-->>OPS: BuilderPayload
    ELS-->>OPS: ExecutionPayload

    OPS-->>OPS: SimulatePayloads
    OPS-->>OPS: ConfirmPayload
    OPS ->> ELS: engine_forkchoiceUpdated

    %% Block propagation to peers
    OPS->>OPB: Peer newly proposed block
```

The proposer and builder uses ECDSA secp256k1 signatures to authenticate the proposer and verify the authenticity of the payload from the builder.


TODO: Add alternative sequence diagram

- Sequencer EL streams fcu/payload attributes
- Builders subscribe, build block and send builder payload directly to the sequencer EL for a given payload id
- Sequencer EL verifies the payload id, verifies the block and evaluates if it is better than the best block
- Sequencer CL eventually sends `getPayload` request. Sequencer EL returns `executionPayload` response containing best block at time of request. If no blocks have been sent to the sequencer EL, the default block is returned to CL.
