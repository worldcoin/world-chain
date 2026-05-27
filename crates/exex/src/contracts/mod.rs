//! Contract bindings for the OP Proposer.
//!
//! Mirrors `op-proposer/contracts/disputegamefactory.go`. We only bind the
//! subset of the ABI the proposer actually uses:
//!
//! * `DisputeGameFactory.gameCount()`
//! * `DisputeGameFactory.gameAtIndex(uint256)`
//! * `DisputeGameFactory.initBonds(uint32)`
//! * `DisputeGameFactory.create(uint32,bytes32,bytes)` — payable
//! * `DisputeGameFactory.version()`
//! * `FaultDisputeGame.claimData(uint256)`

use std::{sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_sol_types::{SolCall, sol};
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

/// Metadata for a game returned by `gameAtIndex` (+ a follow-up claimData lookup).
#[derive(Debug, Clone)]
pub struct GameMetadata {
    pub game_type: u32,
    pub timestamp: u64,
    pub address: Address,
    pub proposer: Address,
    pub claim: B256,
}

/// Thin wrapper around the `DisputeGameFactory` RPC binding that adds
/// per-call network timeouts (matching the Go implementation).
pub struct DisputeGameFactory<P: Provider> {
    address: Address,
    provider: Arc<P>,
    network_timeout: Duration,
}

impl<P: Provider> DisputeGameFactory<P> {
    pub fn new(address: Address, provider: Arc<P>, network_timeout: Duration) -> Self {
        Self {
            address,
            provider,
            network_timeout,
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Returns the factory contract version string.
    pub async fn version(&self) -> Result<String, ContractError> {
        let call = IDisputeGameFactory::versionCall {}.abi_encode();
        let out = self.eth_call(self.address, call.into()).await?;
        let decoded = IDisputeGameFactory::versionCall::abi_decode_returns(&out)
            .map_err(|e| ContractError::Decode(e.to_string()))?;
        Ok(decoded)
    }

    /// Returns the total number of games created by the factory.
    pub async fn game_count(&self) -> Result<u64, ContractError> {
        let call = IDisputeGameFactory::gameCountCall {}.abi_encode();
        let out = self.eth_call(self.address, call.into()).await?;
        let decoded = IDisputeGameFactory::gameCountCall::abi_decode_returns(&out)
            .map_err(|e| ContractError::Decode(e.to_string()))?;
        u64::try_from(decoded).map_err(|_| ContractError::Decode("game count > u64".into()))
    }

    /// Returns the `_index`-th game (type, timestamp, proxy address).
    pub async fn game_at_index_raw(
        &self,
        index: u64,
    ) -> Result<(u32, u64, Address), ContractError> {
        let call = IDisputeGameFactory::gameAtIndexCall {
            _index: U256::from(index),
        }
        .abi_encode();
        let out = self.eth_call(self.address, call.into()).await?;
        let decoded = IDisputeGameFactory::gameAtIndexCall::abi_decode_returns(&out)
            .map_err(|e| ContractError::Decode(e.to_string()))?;
        Ok((decoded.gameType_, decoded.timestamp_, decoded.proxy_))
    }

    /// Returns the `_index`-th game with claimant and claim resolved via a
    /// follow-up `claimData(0)` call. Mirrors the Go `gameAtIndex`.
    pub async fn game_at_index(&self, index: u64) -> Result<GameMetadata, ContractError> {
        let (game_type, timestamp, proxy) = self.game_at_index_raw(index).await?;
        let claim_call = IFaultDisputeGame::claimDataCall { _index: U256::ZERO }.abi_encode();
        let out = self.eth_call(proxy, claim_call.into()).await?;
        let decoded = IFaultDisputeGame::claimDataCall::abi_decode_returns(&out)
            .map_err(|e| ContractError::Decode(e.to_string()))?;
        Ok(GameMetadata {
            game_type,
            timestamp,
            address: proxy,
            proposer: decoded.claimant,
            claim: decoded.claim,
        })
    }

    /// Initial bond required to create a game of the given type.
    pub async fn init_bond(&self, game_type: u32) -> Result<U256, ContractError> {
        let call = IDisputeGameFactory::initBondsCall {
            _gameType: game_type,
        }
        .abi_encode();
        let out = self.eth_call(self.address, call.into()).await?;
        let decoded = IDisputeGameFactory::initBondsCall::abi_decode_returns(&out)
            .map_err(|e| ContractError::Decode(e.to_string()))?;
        Ok(decoded)
    }

    /// Returns true if `proposer` has created a game of type `game_type` after `cutoff_unix`.
    /// On match, also returns the timestamp and the claim (root) of that game.
    ///
    /// Walks the factory backwards from the latest game and short-circuits as
    /// soon as a game older than `cutoff_unix` is seen. Direct port of
    /// `HasProposedSince`.
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

    /// Returns the ABI-encoded `create(uint32,bytes32,bytes)` input and the
    /// required bond (the `value` field of the eventual L1 tx).
    pub async fn proposal_tx_calldata(
        &self,
        game_type: u32,
        output_root: B256,
        extra_data: Bytes,
    ) -> Result<(Bytes, U256), ContractError> {
        let bond = self.init_bond(game_type).await?;
        let call = IDisputeGameFactory::createCall {
            _gameType: game_type,
            _rootClaim: output_root,
            _extraData: extra_data,
        }
        .abi_encode();
        Ok((call.into(), bond))
    }

    async fn eth_call(&self, to: Address, data: Bytes) -> Result<Bytes, ContractError> {
        use alloy_rpc_types_eth::TransactionRequest;
        let req = TransactionRequest::default().to(to).input(data.into());
        let fut = self.provider.call(req);
        let out = tokio::time::timeout(self.network_timeout, fut)
            .await
            .map_err(|_| ContractError::Timeout)?
            .map_err(|e| ContractError::Rpc(e.to_string()))?;
        Ok(out)
    }
}

impl DisputeGameFactory<RootProvider> {
    /// Convenience constructor for the type-erased root provider.
    pub fn from_root(
        address: Address,
        provider: Arc<RootProvider>,
        network_timeout: Duration,
    ) -> Self {
        Self::new(address, provider, network_timeout)
    }
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("rpc call failed: {0}")]
    Rpc(String),
    #[error("call timed out")]
    Timeout,
    #[error("abi decode error: {0}")]
    Decode(String),
}
