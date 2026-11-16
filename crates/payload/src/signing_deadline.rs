use reth_payload_builder::{
    BuildNewPayload, PayloadBuilderLease, PayloadId, PayloadJob, PayloadJobGenerator,
};
use reth_payload_primitives::{PayloadAttributes, PayloadBuilderError};
use reth_primitives_traits::NodePrimitives;
use reth_provider::CanonStateNotification;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use world_chain_builder::signing_deadline::SigningDeadlines;

/// Basic Reth jobs retain this budget through their existing resource leases.
pub struct DeadlineGenerator<G> {
    inner: G,
    deadlines: Option<SigningDeadlines>,
    duration: Duration,
}

impl<G> DeadlineGenerator<G> {
    pub fn new(inner: G, deadlines: Option<SigningDeadlines>, duration: Duration) -> Self {
        Self {
            inner,
            deadlines,
            duration,
        }
    }
}

impl<G: PayloadJobGenerator> PayloadJobGenerator for DeadlineGenerator<G> {
    type Job = G::Job;
    fn new_payload_job(
        &self,
        mut input: BuildNewPayload<<Self::Job as PayloadJob>::PayloadAttributes>,
        id: PayloadId,
    ) -> Result<Self::Job, PayloadBuilderError> {
        if let Some(deadlines) = &self.deadlines {
            // Start before parent lookup: this cannot extend Reth's later internal deadline.
            let now = Instant::now();
            let timestamp = UNIX_EPOCH
                .checked_add(Duration::from_secs(input.attributes.timestamp()))
                .ok_or_else(|| {
                    PayloadBuilderError::other(std::io::Error::other("payload timestamp overflow"))
                })?;
            let until = now
                + self.duration
                + timestamp
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    .min(self.duration * 3);
            let lease = deadlines
                .register(id, until)
                .map_err(|error| PayloadBuilderError::other(std::io::Error::other(error)))?;
            input.resources = input.resources.with_lease(PayloadBuilderLease::new(lease));
        }
        self.inner.new_payload_job(input, id)
    }
    fn on_new_state<N: NodePrimitives>(&mut self, state: CanonStateNotification<N>) {
        self.inner.on_new_state(state);
    }
}
