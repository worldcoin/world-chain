# World Chain Builder

 
## Builder API
<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->
**Table of Contents**

- [Overview](#overview)
- [Sequencer Builder Interaction](#sequencer-builder-interaction)
  - [Liveness Failsafe](#liveness-failsafe)
  - [Mempool Forwarding](#mempool-forwarding)
  - [Builder Configuration](#builder-configuration)
- [Structures](#structures)
  - [`BuilderAttributesV1`](#builderattributesv1)
  - [`BuilderPayloadV1`](#builderpayloadv1)
- [Methods](#methods)
  - [`builder_newPayloadV1`](#builder_newpayloadV1)
  - [`builder_subscribeBuilderAttributes`](#builder_subscribebuilderattributes)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Overview

This document provides an in-depth specification for integrating a Builder API within the Optimism Protocol
and Stack. The Builder API provides a standardized interface for block construction and transaction management
between the Sequencer and a local Block Builder. The specified interactions are the minimum viable design
needed to allow a local Block Builder.

By decoupling the block construction process from the Sequencer's Execution Engine, operators can tailor transaction
sequencing rules without diverging from the standard Optimism Protocol Client. This flexibility allows individual chains
to experiment on sequencing features, providing a means for differentiation. This minimum viable design also includes
a local block production fallback as a training wheel to ensure liveness and network performance in the event of
local Block Builder failure.

It is important to note that this document ***does not*** outline how to safely enable a permissionless
Block Builder Network to serve payloads or how to manage Block Builder configs on L1.

## Sequencer Builder Interaction
```mermaid
sequenceDiagram
    participant CLS as Op-Node (Sequencer)
    participant ELS as Execution Client (Sequencer)
    participant ELB as Execution Client (Builder)
    participant CLB as Op-Node (Builder)

    ELS->>ELS: Start `BuilderAttribute` stream

    Note over ELB: Subscribe to new BuilderAttributes from the sequencer
    ELB->>ELS: builder_subscribeBuilderAttributes

    CLS->>ELS: engine_forkchoiceUpdatedV3(forkchoiceState, PayloadAttributes)
    ELS-->>ELB: emit `BuilderAttribute` event
    ELB->>ELB: Prepare to build block for `payloadId`
    ELS->>ELS: Start building block for `payloadId`

    Note over CLB: New block is peered from CL
    CLB->>ELB: engine_newPayloadV3()
    CLB->>ELB: engine_forkchoiceUpdatedV3(forkchoiceState, null)

    ELB->>ELB: Build block for `payloadId`
    ELB->>ELS: builder_newPayloadV1(BuilderPayloadV1)
    ELS->>ELS: Simulate payload and update best block

    CLS->>ELS: engine_getPayloadV3()
    Note over ELS: Propose the best block from all proposed blocks
    ELS->>CLS: ExecutionPayload

    CLS->>CLS: Validate payload
    Note over CLS: Publish new block and peer to the network
```


- **BuilderAttribute Stream**: Upon initialization, the Sequencer EL starts a `BuilderAttribute` stream, enabling external block builders to consume the update and prepare to construct a new block for a given `payloadId`.

- **Fork Choice Update**: The Sequencer CL sends a Fork Choice Update to its EL (including `PayloadAttributes`), indicating an update to the chain's latest head. The EL then publishes a `BuilderAttributesV1` event to the stream.

- **Send Builder Payload**: After the builder consumes the latest `BuilderAttributesV1` from the sequencer, it will start to prepare a new block inserting the txs from the `transactions` field included in the `PayloadAttributes`. Once the builder EL receives an `ExecutionPayload` from its CL where the `blockHash` matches the `headBlockHash` from the FCU, it will finish constructing a new block and send a `BuilderPayloadV1` message to the sequencer EL via the `builder_newPayload` endpoint. Note that if the builder is able to construct a better block than previously submitted, they can submit multiple blocks for a given `payloadId`. The Sequencer ***MUST*** simulate the received payload to ensure correctness until an accountability mechanism can be introduced.

- **Propose New Block**: The Sequencer EL will continuously receive/validate new blocks via the `builder_newPayload` endpoint until the CL sends a `engine_newPayload` request. At this point, the EL will return the best block that was received for a given `payloadId`.


### Liveness Failsafe

To maintain network liveness while utilizing the Builder API, the Sequencer ***MUST*** operate an auxiliary process when building blocks. This process concurrently evaluates newly built blocks recieved via the `builder_newPayload` enpoint alongside a local block production request through its local execution engine. This two-pronged strategy for generating blocks ensures that network liveness persists, even in instances where the Block Builder's block construction process experiences delays or is offline. This fallback mechanism should be seen as a training wheel.

### Mempool Forwarding

A builder network's throughput is conditional on the transactions it sees. Thus the Sequencer's Execution Engine, or simply it's RPC, can forward transactions to the Builder as part of regular mempool management, ensuring that user transactions are included in the Block Builder's block construction process efficiently.

### Builder Configuration

A builder is defined as the tuple (`builderPubkey`, `builderUrl`). The Sequencer is responsible for managing this tuple, but it will eventually live on the L1 [`SystemConfig`](https://github.com/ethereum-optimism/specs/blob/main/specs/protocol/system-config.md)
where changes are emitted as an event. ***Builder's have no restriction or policies enforced on them at this time.***

## Structures

### `BuilderAttributesV1`
This structure contains information necessary start constructing a new block for a given `payloadId`.

- `forkChoiceUpdate`: [`ForkChoiceStateV1`](https://specs.optimism.io/protocol/exec-engine.html#engine_forkchoiceupdatedv3)
- `payloadAttributes`: [`PayloadAttributesV3`](https://specs.optimism.io/protocol/exec-engine.html#extended-payloadattributesv3)
- `payloadId`: `DATA`, 8 Bytes - Identifier of the payload build process

### `BuilderPayloadV1`

This structure represents the Block Builder's response to the request for payload.

- `executionPayload`: [`ExecutionPayloadV2`](https://github.com/ethereum/execution-apis/blob/main/src/engine/shanghai.md#executionpayloadv2)
- `pubKey`: `Address`
- `value`: `uint256` 


## Methods

### `builder_newPayloadV1`

**Request**

- **method**: `builder_newPayloadV1`
- **params**:
    1. `BuilderPayloadV1`
        - **Required**: true
        - **Description**: The payload constructed by the Builder's Execution Client. This includes:
          - `executionPayload`: The set of transactions and other block-level data that constitutes the block being proposed.
          - `pubKey`: The public key of the Builder responsible for constructing the payload.
          - `value`: The associated block reward value, if applicable.
- **timeout**: 200ms
    - The request should time out after 200ms. If the Sequencer fails to receive the payload within this time frame, it will continue using the local block construction to ensure liveness.
- **retries**: 0
    - No retries are performed for this method as the Sequencer must use the locally constructed block if the Builder fails to provide a valid payload within the given timeout.

**Response**

- **result**: `bool`
    - **Description**: A boolean indicating whether the payload was successfully received and processed by the Sequencer's Execution Client (EL).
        - `true`: The payload was successfully received and processed.
        - `false`: The payload was rejected or encountered an error during validation.
- **error**: Code and message in case an error occurs during payload submission.

**Specification**

1. `builder_newPayloadV1` is an RPC method used by the Builder's Execution Client to submit a new execution payload to the Sequencer's Execution Client (EL). 
2. The Sequencer's Execution Client **MUST** validate and simulate each execution payload received by a builder.
3. The Sequencer **MUST** maintain liveness by continuously receiving and validating new payloads from the Builder until the `engine_newPayload` call is invoked by the Sequencer's Consensus Layer.
4. The best block **MUST** be chosen from all submitted payloads for a given `payloadId`. If the timeout is reached without a valid payload, the Sequencer will fallback to the locally constructed block.


### `builder_subscribeBuilderAttributes`

**Specification**:

1. When the sequencer is running with PBS enabled, upon initialization the Sequencer's Execution Client **MUST** start streaming `BuilderAttributesV1` events.
2. Each `BuilderAttributesV1` event contains:
   - `forkChoiceUpdate`: The updated fork choice state from the Sequencer's Consensus Layer, indicating the latest chain head and fork status.
   - `payloadAttributes`: The attributes required to build a new block.
   - `payloadId`: A unique identifier for the current pending block.
3. The Builder **MUST** subscribe to this stream using the following RPC method:
    - **method**: `builder_subscribeBuilderAttributes`
    - **params**: None
    - **Response**: Confirmation that the Builder has successfully subscribed to the `BuilderAttributes` stream.
        - **result**: `bool`
            - `true`: Successfully subscribed.
            - `false`: Subscription failed.
