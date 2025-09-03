use std::future::Future;

use reth_node_api::{FullNodeComponents, NodeAddOns};

pub struct FlashblocksAddOns<T> {
    pub inner: T,
}

pub struct FlashblocksHandle<T> {
    inner: T,
}

impl<N, T> NodeAddOns<N> for FlashblocksAddOns<T>
where
    T: Send,
    N: FullNodeComponents,
{
    type Handle = ();

    fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> impl Future<Output = eyre::eyre::Result<Self::Handle>> + Send {
        async { todo!() }
    }
}
