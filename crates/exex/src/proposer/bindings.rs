//! Contract bindings for the OP Proposer.
//!
//! Mirrors: [`op-proposer/contracts/disputegamefactory.go`][src] @ tag
//! `op-proposer/v1.16.3-rc.1`. Every wrapper method below references the
//! upstream Go line range it ports.
//!
//! [src]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use alloy_provider::DynProvider;
use alloy_sol_types::sol;
use thiserror::Error;

sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IDisputeGameFactory {
        function gameCount() external view returns (uint256);
        function gameAtIndex(uint256 _index)
            external
            view
            returns (uint32 gameType_, uint64 timestamp_, address proxy_);
        function initBonds(uint32 _gameType) external view returns (uint256);
        function create(uint32 _gameType, bytes32 _rootClaim, bytes calldata _extraData)
            external
            payable
            returns (address proxy_);
        function version() external view returns (string memory);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IFaultDisputeGame {
        // ClaimData layout in upstream contracts is
        //   (uint32 parentIndex, address counteredBy, address claimant,
        //    uint128 bond, Claim claim, Position position, Clock clock)
        // The proposer only needs the `claimant` (proposer) and `claim` (root).
        function claimData(uint256 _index)
            external
            view
            returns (
                uint32 parentIndex,
                address counteredBy,
                address claimant,
                uint128 bond,
                bytes32 claim,
                uint128 position,
                uint128 clock
            );
    }
}

/// Concrete type for the sol-generated factory instance over our dyn provider.
pub type FactoryInstance = IDisputeGameFactory::IDisputeGameFactoryInstance<DynProvider>;

/// Metadata for a game returned by `gameAtIndex` (+ a follow-up claimData lookup).
#[derive(Debug, Clone)]
pub struct GameMetadata {
    pub game_type: u32,
    pub timestamp: u64,
    pub address: Address,
    pub proposer: Address,
    pub claim: B256,
}

/// Thin wrapper around the sol-generated factory instance.
///
/// Keeps [`has_proposed_since`](Self::has_proposed_since) — the only piece
/// of business logic on top of the raw ABI calls — and re-exposes the
/// underlying instance for `.create(..).send()` from the driver.
#[derive(Clone)]
pub struct DisputeGameFactory {
    instance: Arc<FactoryInstance>,
}

impl DisputeGameFactory {
    pub fn new(address: Address, provider: DynProvider) -> Self {
        let instance = IDisputeGameFactory::new(address, provider);
        Self {
            instance: Arc::new(instance),
        }
    }

    /// The DGF address.
    pub fn address(&self) -> Address {
        *self.instance.address()
    }

    /// Underlying alloy contract instance (use for `.create(..).send()`).
    pub fn instance(&self) -> &FactoryInstance {
        &self.instance
    }

    /// Returns the factory `version()` string.
    ///
    /// Mirrors: `(*DisputeGameFactory).Version` in
    /// [disputegamefactory.go L57–L65][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go#L57-L65
    pub async fn version(&self) -> Result<String, ContractError> {
        Ok(self.instance.version().call().await?)
    }

    /// Total number of games created by the factory.
    ///
    /// Mirrors: `(*DisputeGameFactory).gameCount` in
    /// [disputegamefactory.go L115–L123][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go#L115-L123
    pub async fn game_count(&self) -> Result<u64, ContractError> {
        let n = self.instance.gameCount().call().await?;
        u64::try_from(n).map_err(|_| ContractError::Decode("game count > u64".into()))
    }

    /// `(game_type, timestamp, proxy)` for the i-th game.
    pub async fn game_at_index_raw(
        &self,
        index: u64,
    ) -> Result<(u32, u64, Address), ContractError> {
        let ret = self.instance.gameAtIndex(U256::from(index)).call().await?;
        Ok((ret.gameType_, ret.timestamp_, ret.proxy_))
    }

    /// `game_at_index_raw` + a follow-up `claimData(0)` to resolve the
    /// claimant / claim.
    ///
    /// Mirrors: `(*DisputeGameFactory).gameAtIndex` in
    /// [disputegamefactory.go L125–L154][src].
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go#L125-L154
    pub async fn game_at_index(&self, index: u64) -> Result<GameMetadata, ContractError> {
        let (game_type, timestamp, proxy) = self.game_at_index_raw(index).await?;
        let game = IFaultDisputeGame::new(proxy, self.instance.provider().clone());
        let ret = game.claimData(U256::ZERO).call().await?;
        Ok(GameMetadata {
            game_type,
            timestamp,
            address: proxy,
            proposer: ret.claimant,
            claim: ret.claim,
        })
    }

    /// Initial bond required to create a game of the given type.
    ///
    /// Mirrors the `initBonds` call wrapped in
    /// `(*DisputeGameFactory).ProposalTx` —
    /// [disputegamefactory.go L98–L113][src] L100–L106. We hoist the read
    /// into its own method since alloy's contract instance handles the
    /// calldata + value assembly for us.
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go#L98-L113
    pub async fn init_bond(&self, game_type: u32) -> Result<U256, ContractError> {
        Ok(self.instance.initBonds(game_type).call().await?)
    }

    /// Returns true if `proposer` has created a game of type `game_type`
    /// after `cutoff_unix`, along with the timestamp and claim of that game.
    ///
    /// Mirrors: `(*DisputeGameFactory).HasProposedSince` in
    /// [disputegamefactory.go L70–L96][src]. The Rust loop is a 1:1
    /// translation of the Go for-loop that walks the factory backwards
    /// from the latest game and short-circuits on the cutoff.
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-proposer/contracts/disputegamefactory.go#L70-L96
    pub async fn has_proposed_since(
        &self,
        proposer: Address,
        cutoff_unix: u64,
        game_type: u32,
    ) -> Result<Option<(u64, B256)>, ContractError> {
        let count = self.game_count().await?;
        if count == 0 {
            return Ok(None);
        }
        let mut idx = count;
        loop {
            idx -= 1;
            let game = self.game_at_index(idx).await?;
            if game.timestamp < cutoff_unix {
                return Ok(None);
            }
            if game.game_type == game_type && game.proposer == proposer {
                return Ok(Some((game.timestamp, game.claim)));
            }
            if idx == 0 {
                return Ok(None);
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error(transparent)]
    Alloy(#[from] alloy_contract::Error),
    #[error("abi decode error: {0}")]
    Decode(String),
}
