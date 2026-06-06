use std::fmt;

/// Container image reference used by devnet components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerImage {
    /// Repository/name portion of the image.
    pub repository: String,
    /// Image tag.
    pub tag: String,
}

impl ContainerImage {
    /// Build a new image reference from repository and tag.
    pub fn new(repository: impl Into<String>, tag: impl Into<String>) -> Self {
        Self {
            repository: repository.into(),
            tag: tag.into(),
        }
    }

    /// Fully qualified `repository:tag` reference.
    pub fn reference(&self) -> String {
        format!("{}:{}", self.repository, self.tag)
    }
}

impl fmt::Display for ContainerImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reference())
    }
}

/// High-level component categories in the native World Chain devnet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DevnetComponentKind {
    /// L1 execution dev chain.
    L1DevChain,
    /// OP contract deployment step.
    OpContractDeployer,
    /// World Chain L2 execution node.
    WorldChainExecutionNode,
    /// OP Node / rollup consensus component.
    OpNode,
    /// OP Conductor HA sequencer coordinator.
    OpConductor,
    /// OP Batcher.
    OpBatcher,
    /// OP Proposer.
    OpProposer,
    /// OP Challenger.
    OpChallenger,
    /// Prometheus metrics scraper.
    Prometheus,
    /// Grafana dashboards.
    Grafana,
    /// World Chain contract deployment step.
    WorldContractsDeployer,
    /// World Chain WIP-1006 proof-system contracts on L1.
    WorldProofSystem,
    /// World Chain WIP-1006 proof-system proposer.
    WorldChainProposer,
    /// World Chain WIP-1006 proof-system challenger.
    WorldChainChallenger,
    /// Flashblocks capability on the World Chain execution node.
    Flashblocks,
    /// Deprecated/removed legacy component.
    RemovedLegacyService,
}

impl DevnetComponentKind {
    /// Stable lowercase identifier for logs and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::L1DevChain => "l1-dev-chain",
            Self::OpContractDeployer => "op-contract-deployer",
            Self::WorldChainExecutionNode => "world-chain-execution-node",
            Self::OpNode => "op-node",
            Self::OpConductor => "op-conductor",
            Self::OpBatcher => "op-batcher",
            Self::OpProposer => "op-proposer",
            Self::OpChallenger => "op-challenger",
            Self::Prometheus => "prometheus",
            Self::Grafana => "grafana",
            Self::WorldContractsDeployer => "world-contracts-deployer",
            Self::WorldProofSystem => "world-proof-system",
            Self::WorldChainProposer => "world-chain-proposer",
            Self::WorldChainChallenger => "world-chain-challenger",
            Self::Flashblocks => "flashblocks",
            Self::RemovedLegacyService => "removed-legacy-service",
        }
    }
}

/// Lifecycle state of a component in a particular devnet run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevnetComponentStatus {
    /// Started and owned by the current `WorldDevnet`.
    Running,
    /// Part of the selected topology but not started by the current prototype.
    Planned,
    /// Explicitly deferred until a dependency is implemented.
    Deferred,
    /// Intentionally absent from the new topology.
    Removed,
}

impl DevnetComponentStatus {
    /// Stable lowercase identifier for logs and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Planned => "planned",
            Self::Deferred => "deferred",
            Self::Removed => "removed",
        }
    }
}

/// Public endpoint exposed by a devnet component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevnetEndpoint {
    /// Endpoint name, such as `rpc` or `metrics`.
    pub name: String,
    /// Endpoint URL.
    pub url: String,
}

impl DevnetEndpoint {
    /// Build a named endpoint.
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
        }
    }
}

/// Typed component entry shown by `xtask devnet up` and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevnetComponent {
    /// Stable component ID within the devnet.
    pub id: String,
    /// Component category.
    pub kind: DevnetComponentKind,
    /// Runtime state.
    pub status: DevnetComponentStatus,
    /// Container image, when the component is container-backed.
    pub image: Option<ContainerImage>,
    /// Public endpoints.
    pub endpoints: Vec<DevnetEndpoint>,
    /// Short operational notes.
    pub notes: Vec<String>,
}

impl DevnetComponent {
    /// Start a component manifest entry.
    pub fn new(
        id: impl Into<String>,
        kind: DevnetComponentKind,
        status: DevnetComponentStatus,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            status,
            image: None,
            endpoints: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Attach a container image reference.
    pub fn with_image(mut self, image: ContainerImage) -> Self {
        self.image = Some(image);
        self
    }

    /// Attach a public endpoint.
    pub fn with_endpoint(mut self, name: impl Into<String>, url: impl Into<String>) -> Self {
        self.endpoints.push(DevnetEndpoint::new(name, url));
        self
    }

    /// Attach an operational note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}
