# PBH Architecture
World Chain is an OP Stack chain that enables Priority Blockspace for Humans (PBH) through the World Chain Builder. World Chain leverages [rollup-boost](https://github.com/flashbots/rollup-boost) to support external block production, allowing the builder to propose PBH blocks to the sequencer while remaining fully compatible with the OP Stack.

 
 ## Block Production on the OP Stack
 The [Engine API](https://specs.optimism.io/protocol/exec-engine.html#engine-api) defines the communication protocol between the Consensus Layer (CL) and the Execution Layer (EL) and is responsible for orchestrating block production on the OP Stack. Periodically, the sequencer's consensus client will send a fork choice update (FCU) to its execution client, signaling for a new block to be built. After a series of API calls between the CL and EL, the EL will return a new `ExecutionPayload` containing a newly constructed block. The CL will then advance the unsafe head of the chain and peer the new block to other nodes in the network.
 

```mermaid
sequenceDiagram
    box OP Stack Sequencer
        participant sequencer-cl as Sequencer CL
        participant sequencer-el as Sequencer EL
    end
    box Network
        participant peers-cl as Peers CL
    end

    Note over sequencer-cl: FCU with Attributes
    sequencer-cl->>sequencer-el: engine_forkChoiceUpdatedV3(ForkChoiceState, Attrs)
    sequencer-el-->>sequencer-cl: {payloadStatus: {status: VALID, ...}, payloadId: PayloadId}
    Note over sequencer-el: Build execution payload
    sequencer-cl->>sequencer-el: engine_getPayloadV3(PayloadId)
    sequencer-el-->>sequencer-cl: {executionPayload, blockValue}
    sequencer-cl->>peers-cl: Propagate new block


```


 For a detailed look at how block production works on the OP Stack, see the [OP Stack specs](https://specs.optimism.io/protocol/exec-engine.html#engine-api).




 ## Rollup Boost
`rollup-boost` is a block building sidecar for OP Stack chains, enabling external block production while remaining fully compatible with the OP Stack. `rollup-boost` acts as an intermediary between the sequencer's consensus and execution client. When `sequencer-cl` sends a new FCU to `rollup-boost`, the request will be multiplexed to both the sequencer's execution client and external block builders signaling that a new block should be built. 

When the sequencer is ready to propose a new block, `sequencer-cl` will send an `engine_getPayload` request to `rollup-boost` which is forwarded to the default execution client and external block builders. Note that `rollup-boost` will always fallback to the default execution client's block in the case that the external builder does not respond in time or returns an invalid block. 

Once `rollup-boost` receives the external builder's block, it will then validate the block by sending it to the sequencer's execution client via `engine_newPayload`. If the external block is valid, it is returned to the sequencer `op-node`, otherwise, `rollup-boost` will return the fallback block.

```mermaid
sequenceDiagram
    box Sequencer
        participant sequencer-cl as Sequencer CL
        participant rollup-boost
        participant sequencer-el as Sequencer EL
    end
    box Builder
        participant builder-el as Builder EL
    end

    Note over sequencer-cl: FCU with Attributes
    sequencer-cl->>rollup-boost: engine_forkChoiceUpdatedV3(..., Attrs)

    Note over rollup-boost: Forward FCU
    rollup-boost->>builder-el: engine_forkChoiceUpdatedV3(..., Attrs)

    rollup-boost->>sequencer-el: engine_forkChoiceUpdatedV3(..., Attrs)
    sequencer-el-->>rollup-boost: {payloadId: PayloadId}
    rollup-boost-->>sequencer-cl: {payloadId: PayloadId}


    Note over sequencer-cl: Get Payload
    sequencer-cl->>rollup-boost: engine_getPayloadV3(PayloadId)
    Note over rollup-boost: Forward Get Payload
    rollup-boost->>sequencer-el: engine_getPayloadV3(PayloadId)
    rollup-boost->>builder-el: engine_getPayloadV3(PayloadId)
    builder-el-->>rollup-boost: {executionPayload, blockValue}
    sequencer-el-->>rollup-boost: {executionPayload, blockValue}



    Note over rollup-boost, sequencer-el: Validate builder block
    rollup-boost->>sequencer-el: engine_newPayloadV3(ExecutionPayload)
    sequencer-el->>rollup-boost: {status: VALID, ...}

    Note over rollup-boost: Propose exectuion payload
    rollup-boost->>sequencer-cl: {executionPayload, blockValue}
    
    Note over sequencer-cl: Propagate new block
```


In addition to Engine API requests, `rollup-boost` will proxy all RPC calls from the sequencer `op-node` to its local execution client. The following RPC calls will also be forwarded to external builders:
- `miner_*`
    - The Miner API is used to notify execution clients of changes in effective gas price, extra data, and DA throttling requests from the batcher.
- `eth_sendRawTransaction*`
    - Forwards transactions the sequencer receives to the builder for block building.
 
 </br>
 
 ## Block Production on World Chain

World Chain leverages `rollup-boost` to enable external block production and integrates the World Chain Builder as a block builder in the network. The World Chain Builder implements a custom block ordering policy (ie. PBH) to provide priority inclusion for transactions with a valid World ID proof. Note that the custom ordering policy adheres to the OP Stack spec. 


<!-- TODO: insert Default block vs PBH block -->


In the event that the block builder is offline, `rollup-boost` will fallback to the block built by the default execution client with standard OP Stack ordering rules.

