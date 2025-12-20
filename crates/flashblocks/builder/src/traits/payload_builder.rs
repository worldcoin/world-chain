use flashblocks_primitives::access_list::FlashblockAccessList;
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, PayloadBuilder};
use reth_node_api::PayloadBuilderError;

pub trait FlashblockPayloadBuilder: PayloadBuilder + Send + Sync + Clone {
    /// Tries to build a transaction payload using provided arguments.
    ///
    /// Constructs a transaction payload based on the given arguments,
    /// returning a `Result` indicating success or an error if building fails.
    ///
    /// # Arguments
    ///
    /// - `args`: Build arguments containing necessary components.
    ///
    /// # Returns
    ///
    /// A `Result` indicating the build outcome, and block access list or an error.
    fn try_build_with_precommit(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
        committed_payload: Option<&Self::BuiltPayload>,
    ) -> Result<
        (
            BuildOutcome<Self::BuiltPayload>,
            Option<FlashblockAccessList>,
        ),
        PayloadBuilderError,
    >;
}
