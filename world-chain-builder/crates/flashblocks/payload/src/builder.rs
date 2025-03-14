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

// impl<P: PayloadBuilder> FlashBlocksPayloadBuilder<P> {
//     /// `OpPayloadBuilder` constructor.
//     pub fn new(inner: P) -> Self {
//         let (tx, rx) = mpsc::unbounded_channel();
//         let subscribers = Arc::new(Mutex::new(Vec::new()));

//         Self::publish_task(rx, subscribers.clone());

//         tokio::spawn(async move {
//             Self::start_ws(subscribers, "127.0.0.1:1111").await;
//         });

//         Self { inner, tx }
//     }
//     /// Start the WebSocket server
//     pub async fn start_ws(subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>, addr: &str) {
//         let listener = TcpListener::bind(addr).await.unwrap();
//         let subscribers = subscribers.clone();

//         tracing::info!("Starting WebSocket server on {}", addr);

//         while let Ok((stream, _)) = listener.accept().await {
//             tracing::info!("Accepted websocket connection");
//             let subscribers = subscribers.clone();

//             tokio::spawn(async move {
//                 match accept_async(stream).await {
//                     Ok(ws_stream) => {
//                         let mut subs = subscribers.lock().unwrap();
//                         subs.push(ws_stream);
//                     }
//                     Err(e) => eprintln!("Error accepting websocket connection: {}", e),
//                 }
//             });
//         }
//     }

//     /// Background task that handles publishing messages to WebSocket subscribers
//     fn publish_task(
//         mut rx: mpsc::UnboundedReceiver<String>,
//         subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
//     ) {
//         tokio::spawn(async move {
//             while let Some(message) = rx.recv().await {
//                 let mut subscribers = subscribers.lock().unwrap();

//                 // Remove disconnected subscribers and send message to connected ones
//                 subscribers.retain_mut(|ws_stream| {
//                     let message = message.clone();
//                     async move {
//                         ws_stream
//                             .send(tokio_tungstenite::tungstenite::Message::Text(
//                                 message.into(),
//                             ))
//                             .await
//                             .is_ok()
//                     }
//                     .now_or_never()
//                     .unwrap_or(false)
//                 });
//             }
//         });
//     }
// }

// impl<P, N> PayloadBuilder for FlashBlocksPayloadBuilder<P>
// where
//     P: PayloadBuilder,
//     N: NodePrimitives,
// {
//     type Attributes = OpPayloadBuilderAttributes<N::SignedTx>;
//     type BuiltPayload = OpBuiltPayload<N>;

//     fn try_build(
//         &self,
//         args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
//     ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
//         todo!()
//     }

//     fn build_empty_payload(
//         &self,
//         config: reth_basic_payload_builder::PayloadConfig<
//             Self::Attributes,
//             reth_basic_payload_builder::HeaderForPayload<Self::BuiltPayload>,
//         >,
//     ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
//         todo!()
//     }
// }

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
