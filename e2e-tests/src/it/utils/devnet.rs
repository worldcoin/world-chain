//! Shared fixtures for the devnet E2E tests that drive the WIP-1006 proof system.
//!
//! [`devnet_challenge`](crate::it::devnet_challenge),
//! [`devnet_proof_invariants`](crate::it::devnet_proof_invariants) and
//! [`devnet_withdrawal`](crate::it::devnet_withdrawal) all stand up the same HA-sequencer full
//! stack, talk to the same `MultiProofGame` instances, and poll the same on-chain state while the
//! real in-process proposer/challenger/defender services race them. Everything they hold in common
//! lives here; each test keeps only the setup and assertions specific to the property it exercises.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumberOrTag;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder, WalletProvider, ext::AnvilApi};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, bail, ensure, eyre};
use url::Url;
use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WorldDevnet, WorldDevnetBuilder, WorldDevnetPreset,
    is_docker_unavailable,
};
use world_chain_proof_protocol::{
    DEFAULT_L1_TX_RECEIPT_TIMEOUT_SECONDS, IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory,
    IMultiProofGame, MULTI_PROOF_GAME_TYPE,
};
use world_chain_proposer::AlloyProofSystemClient;

/// How long any single wait on devnet-driven on-chain state may take before the test fails.
pub(in crate::it) const GAME_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
pub(in crate::it) const GAME_IN_PROGRESS: u8 = 0;
pub(in crate::it) const GAME_CHALLENGER_WINS: u8 = 1;
pub(in crate::it) const GAME_DEFENDER_WINS: u8 = 2;
/// `LibProof.InvalidationReason` ordinals (`pkg/contracts/src/dispute/lib/LibProof.sol`).
pub(in crate::it) const INVALIDATION_REASON_PROOF_TIMEOUT: u8 = 1;
pub(in crate::it) const INVALIDATION_REASON_INVALID_PARENT: u8 = 2;

/// Funds a throwaway Anvil account well above any bond the factory could demand (100 ETH).
const THROWAWAY_ACCOUNT_BALANCE_WEI: u128 = 100_000_000_000_000_000_000;
const GAME_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Builds the HA-sequencer full stack every proof-system test runs against.
///
/// Returns `Ok(None)` — after printing `skip_label` — when Docker is unavailable, so a developer
/// without a container runtime sees a skip rather than a failure.
pub(in crate::it) async fn try_build_ha_devnet(
    skip_label: &str,
) -> eyre::Result<Option<WorldDevnet>> {
    let ha_config = HaSequencerConfig::default()
        .with_sequencer_count(2)
        .with_observability(ObservabilityConfig::default());

    match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::HaSequencer)
        .ha_sequencer(ha_config)
        .block_time(Duration::from_secs(1))
        .build()
        .await
    {
        Ok(devnet) => Ok(Some(devnet)),
        Err(err) if is_docker_unavailable(&err) => {
            eprintln!("skipping {skip_label} because Docker is unavailable: {err:#}");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// L1 RPC URL of a full-stack devnet.
pub(in crate::it) fn l1_rpc_url(devnet: &WorldDevnet) -> eyre::Result<&str> {
    devnet
        .l1_rpc_url()
        .ok_or_eyre("full-stack devnet missing L1 RPC")
}

/// Parses one of the devnet's optional L1 contract addresses, naming it if absent.
pub(in crate::it) fn l1_contract(address: Option<&str>, what: &str) -> eyre::Result<Address> {
    Ok(address
        .ok_or_else(|| eyre!("full-stack devnet missing {what}"))?
        .parse()?)
}

/// Builds an HTTP provider that signs with `signer`.
///
/// The wallet stays un-erased (no [`alloy_provider::DynProvider`]) because
/// [`AlloyProofSystemClient`]'s proposer traits require [`WalletProvider`], which erasure drops.
pub(in crate::it) fn signing_provider(
    rpc: &str,
    signer: PrivateKeySigner,
) -> eyre::Result<impl Provider + WalletProvider + Clone + use<>> {
    Ok(ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(rpc)?))
}

/// Funds a fresh random account through Anvil's `anvil_setBalance` cheat and returns its address
/// alongside a provider that signs with it.
///
/// Cheating balance in rather than adding the key to the devnet's L1 genesis keeps the shared
/// devnet fixture unchanged for every other test.
pub(in crate::it) async fn funded_throwaway_provider(
    l1_rpc: &str,
) -> eyre::Result<(Address, impl Provider + WalletProvider + Clone + use<>)> {
    let signer = PrivateKeySigner::random();
    let address = signer.address();
    ProviderBuilder::new()
        .connect_http(Url::parse(l1_rpc)?)
        .anvil_set_balance(address, U256::from(THROWAWAY_ACCOUNT_BALANCE_WEI))
        .await?;

    Ok((address, signing_provider(l1_rpc, signer)?))
}

/// Connects a proposer-side proof-system client to the devnet's factory.
pub(in crate::it) async fn proof_system_client<P>(
    provider: P,
    factory_address: Address,
) -> eyre::Result<AlloyProofSystemClient<P>>
where
    P: Provider + Clone,
{
    Ok(AlloyProofSystemClient::new(
        provider,
        factory_address,
        1,
        Duration::from_secs(DEFAULT_L1_TX_RECEIPT_TIMEOUT_SECONDS),
    )
    .await?)
}

/// Binds the `MultiProofGame` at `address`.
pub(in crate::it) fn game_at<P>(
    address: Address,
    provider: P,
) -> IMultiProofGame::IMultiProofGameInstance<P>
where
    P: Provider,
{
    IMultiProofGame::IMultiProofGameInstance::new(address, provider)
}

/// Binds the `AnchorStateRegistry` at `address`.
pub(in crate::it) fn anchor_at<P>(
    address: Address,
    provider: P,
) -> IAnchorStateRegistry::IAnchorStateRegistryInstance<P>
where
    P: Provider,
{
    IAnchorStateRegistry::IAnchorStateRegistryInstance::new(address, provider)
}

/// Binds the `DelayedWETH` at `address`.
pub(in crate::it) fn weth_at<P>(
    address: Address,
    provider: P,
) -> IDelayedWETH::IDelayedWETHInstance<P>
where
    P: Provider,
{
    IDelayedWETH::IDelayedWETHInstance::new(address, provider)
}

/// Polls `probe` until it yields a value, giving up after [`GAME_WAIT_TIMEOUT`].
///
/// `Ok(None)` means "not yet, keep waiting". An `Err` aborts immediately, which is how callers
/// surface terminal states — a game that resolved before it could be challenged is a failure worth
/// reporting now, not after spinning out the full timeout. `what` completes the sentence
/// "timed out after 300s waiting for …".
async fn poll_until<F, Fut, T>(what: &str, mut probe: F) -> eyre::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = eyre::Result<Option<T>>>,
{
    let started = Instant::now();
    loop {
        if let Some(value) = probe().await? {
            return Ok(value);
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out after {GAME_WAIT_TIMEOUT:?} waiting for {what}");
        }
        tokio::time::sleep(GAME_POLL_INTERVAL).await;
    }
}

pub(in crate::it) async fn latest_timestamp<P>(provider: &P) -> eyre::Result<u64>
where
    P: Provider,
{
    Ok(provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await?
        .ok_or_eyre("latest L1 block missing")?
        .header
        .timestamp())
}

/// Warps Anvil's clock to `target` (if it is in the future) and mines a block so the new timestamp
/// is observable on-chain.
pub(in crate::it) async fn advance_to_timestamp<P>(provider: &P, target: u64) -> eyre::Result<()>
where
    P: Provider,
{
    let current = latest_timestamp(provider).await?;
    if target > current {
        provider.anvil_increase_time(target - current).await?;
    }
    provider.evm_mine(None).await?;
    Ok(())
}

/// Waits for the newest WIP-1006 game whose L2 sequence number is at or beyond `min_l2_block`,
/// returning its factory index, address, and L2 sequence number.
///
/// Pass `0` to wait for any WIP-1006 game at all.
pub(in crate::it) async fn wait_for_multi_proof_game<P>(
    provider: P,
    factory_address: Address,
    min_l2_block: u64,
) -> eyre::Result<(u64, Address, u64)>
where
    P: Provider + Clone,
{
    let factory =
        IDisputeGameFactory::IDisputeGameFactoryInstance::new(factory_address, provider.clone());
    let started = Instant::now();

    loop {
        let game_count: u64 = factory.gameCount().call().await?.try_into()?;
        for index in (0..game_count).rev() {
            let entry = factory.gameAtIndex(U256::from(index)).call().await?;
            if entry.gameType != MULTI_PROOF_GAME_TYPE {
                continue;
            }
            let game = game_at(entry.proxy, provider.clone());
            let l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
            if l2_block >= min_l2_block {
                return Ok((index, entry.proxy, l2_block));
            }
        }

        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!(
                "timed out waiting for a respected WIP-1006 game at or beyond L2 block {min_l2_block}"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Waits for `game` to be challenged, returning the challenger's address.
pub(in crate::it) async fn wait_for_challenge<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
) -> eyre::Result<Address>
where
    P: Provider,
{
    let address = *game.address();
    poll_until(&format!("game {address} to be challenged"), || async {
        let challenger = game.challenger().call().await?;
        if challenger != Address::ZERO {
            return Ok(Some(challenger));
        }
        ensure!(
            game.status().call().await? == GAME_IN_PROGRESS,
            "game {address} resolved before it was ever challenged"
        );
        Ok(None)
    })
    .await
}

/// Waits for at least one proof lane to land on `game`.
pub(in crate::it) async fn wait_for_proof_lane<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
) -> eyre::Result<()>
where
    P: Provider,
{
    let address = *game.address();
    poll_until(&format!("a proof lane on game {address}"), || async {
        if game.proofBitmap().call().await? != 0 {
            return Ok(Some(()));
        }
        ensure!(
            game.status().call().await? == GAME_IN_PROGRESS,
            "game {address} resolved before receiving any proof lane"
        );
        Ok(None)
    })
    .await
}

/// Waits for `game` to resolve to `expected`, failing fast if it resolves to anything else.
pub(in crate::it) async fn wait_for_status<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
    expected: u8,
) -> eyre::Result<()>
where
    P: Provider,
{
    let address = *game.address();
    poll_until(
        &format!("game {address} to resolve to status {expected}"),
        || async {
            let status = game.status().call().await?;
            if status == expected {
                return Ok(Some(()));
            }
            ensure!(
                status == GAME_IN_PROGRESS,
                "game {address} resolved with status {status}, expected {expected}"
            );
            Ok(None)
        },
    )
    .await
}

/// Like [`wait_for_status`], but resolves the game itself if nothing else has by the timeout.
///
/// Necessary where no in-process service is guaranteed to be watching the game — a proposal the
/// real challenger never discovers as challengeable will sit `InProgress` forever unless the test
/// drives resolution. Either way, the resolved status must still be `expected`.
pub(in crate::it) async fn wait_for_status_or_resolve<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
    expected: u8,
) -> eyre::Result<()>
where
    P: Provider,
{
    let address = *game.address();
    let started = Instant::now();
    let status = loop {
        let status = game.status().call().await?;
        if status != GAME_IN_PROGRESS {
            break status;
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            resolve_if_still_in_progress(game).await?;
            break game.status().call().await?;
        }
        tokio::time::sleep(GAME_POLL_INTERVAL).await;
    };

    ensure!(
        status == expected,
        "game {address} resolved with status {status}, expected {expected}"
    );
    Ok(())
}

/// Calls `resolve()` unless another actor (e.g. the real challenger's resolution manager) already
/// resolved the game first; either way is fine, these tests only care about the final outcome.
pub(in crate::it) async fn resolve_if_still_in_progress<P>(
    game: &IMultiProofGame::IMultiProofGameInstance<P>,
) -> eyre::Result<()>
where
    P: Provider,
{
    if game.status().call().await? == GAME_IN_PROGRESS {
        let receipt = game.resolve().send().await?.get_receipt().await?;
        ensure!(receipt.status(), "resolve() transaction reverted");
    }
    Ok(())
}

/// Waits for `game_address` to clear the `AnchorStateRegistry`'s finality airgap.
pub(in crate::it) async fn wait_for_game_finality<P>(
    anchor: &IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    game_address: Address,
) -> eyre::Result<()>
where
    P: Provider,
{
    poll_until(
        &format!("game {game_address} to pass the ASR finality airgap"),
        || async {
            Ok(anchor
                .isGameFinalized(game_address)
                .call()
                .await?
                .then_some(()))
        },
    )
    .await
}

/// Waits for the `AnchorStateRegistry` anchor to advance to at least `game_l2_block`.
pub(in crate::it) async fn wait_for_anchor_at_or_beyond<P>(
    anchor: &IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    game_l2_block: u64,
) -> eyre::Result<()>
where
    P: Provider,
{
    poll_until(
        &format!("the ASR anchor to reach L2 block {game_l2_block}"),
        || async {
            let anchor_root = anchor.getAnchorRoot().call().await?;
            Ok((anchor_root.l2SequenceNumber >= U256::from(game_l2_block)).then_some(()))
        },
    )
    .await
}

/// Waits for `proposer`'s bond on `game_address` to be credited in `DelayedWETH`, returning the
/// timestamp at which the withdrawal delay expires.
pub(in crate::it) async fn wait_for_bond_unlock<P>(
    weth: &IDelayedWETH::IDelayedWETHInstance<P>,
    game_address: Address,
    proposer: Address,
) -> eyre::Result<u64>
where
    P: Provider,
{
    let delay = weth.delay().call().await?;
    poll_until("proposer bond credit to unlock", || async {
        let pending = weth.withdrawals(game_address, proposer).call().await?;
        if pending.amount.is_zero() {
            return Ok(None);
        }
        let unlock_at: u64 = pending
            .timestamp
            .checked_add(delay)
            .ok_or_eyre("DelayedWETH unlock timestamp overflow")?
            .try_into()?;
        Ok(Some(unlock_at))
    })
    .await
}

/// Waits for `proposer`'s bond on `game_address` to be fully withdrawn from `DelayedWETH`.
pub(in crate::it) async fn wait_for_bond_withdrawal<P>(
    weth: &IDelayedWETH::IDelayedWETHInstance<P>,
    game_address: Address,
    proposer: Address,
) -> eyre::Result<()>
where
    P: Provider,
{
    poll_until("proposer bond withdrawal", || async {
        Ok(weth
            .withdrawals(game_address, proposer)
            .call()
            .await?
            .amount
            .is_zero()
            .then_some(()))
    })
    .await
}
