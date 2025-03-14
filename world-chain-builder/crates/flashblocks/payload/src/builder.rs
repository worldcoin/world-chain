use std::sync::{Arc, Mutex};

use reth_basic_payload_builder::PayloadBuilder;

// use futures_util::{FutureExt, SinkExt};
// use reth_basic_payload_builder::{BuildArguments, BuildOutcome, PayloadBuilder};
// use reth_chainspec::EthChainSpec;
// use reth_evm::ConfigureEvmFor;
// use reth_node_api::{NodePrimitives, PayloadBuilderError};
// use reth_optimism_forks::OpHardforks;
// use reth_optimism_payload_builder::payload::{OpBuiltPayload, OpPayloadBuilderAttributes};
// use reth_optimism_primitives::OpTransactionSigned;
// use reth_provider::{ChainSpecProvider, StateProviderFactory};
// use reth_transaction_pool::{PoolTransaction, TransactionPool};
// use tokio::{
//     net::{TcpListener, TcpStream},
//     sync::mpsc,
// };
// use tokio_tungstenite::{WebSocketStream, accept_async};

#[derive(Debug, Clone)]
pub struct FlashBlocksPayloadBuilder<P: PayloadBuilder + Flashblocks> {
    inner: P,
}

impl<P> PayloadBuilder for FlashBlocksPayloadBuilder
where
    P: PayloadBuilder,
{
    type Attributes = P::Attributes;
    type BuiltPayload = P::BuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        // TODO: init db

        // TODO: init block builder

        // TODO: apply pre execution changes

        // TODO: execute seqeuencer transactions

        // TODO: dynamically calculate number of flashblocks
        let num_flashblocks = 4;
        for _ in 0..num_flashblocks {

            // TODO: pass in the flashblock ctx and build the next portion of the block

            // TODO: stream the flashblock
        }

        // TODO: flashblock.into() P::BuiltPayload, this should be a trait impl

        // TODO: return the built block
        todo!()
    }

    fn on_missing_payload(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        self.inner.on_missing_payload(args)
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        self.inner.build_empty_payload(config)
    }
}

pub struct Flashblock {}
