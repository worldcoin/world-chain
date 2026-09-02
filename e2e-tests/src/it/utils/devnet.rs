//! Shared fixtures for the devnet E2E tests that drive the WIP-1006 proof system.
//!
//! These tests all stand up the same HA-sequencer stack and poll the same on-chain state while the
//! real proposer/challenger/defender services race them.

use std::{
    future::Future,
    str::FromStr,
    time::{Duration, Instant},
};

use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumberOrTag;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder, WalletProvider, ext::AnvilApi};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use eyre::eyre::{OptionExt, bail, ensure, eyre};
use url::Url;
use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WorldDevnet, WorldDevnetBuilder, WorldDevnetPreset,
    is_docker_unavailable,
};
use world_chain_proof_protocol::{
    DEFAULT_L1_TX_RECEIPT_TIMEOUT_SECONDS, IAnchorStateRegistry, IDisputeGameFactory,
    IERC20StakingVault, IMultiProofGame, MULTI_PROOF_GAME_TYPE, read_registered_bond_vault,
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
/// Mock bond tokens deposited for each throwaway proof-system participant (100 tokens).
const THROWAWAY_ACCOUNT_BOND_TOKEN_BALANCE: u128 = 100_000_000_000_000_000_000;
const GAME_POLL_INTERVAL: Duration = Duration::from_secs(1);

sol! {
    #[sol(rpc)]
    interface IMockBondToken {
        function mint(address recipient, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }
}

pub async fn try_build_ha_devnet_with_custom_block_time(
    skip_label: &str,
    block_time: Duration,
) -> eyre::Result<Option<WorldDevnet>> {
    let ha_config = HaSequencerConfig::default()
        .with_sequencer_count(2)
        .with_observability(ObservabilityConfig::default());

    match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::HaSequencer)
        .ha_sequencer(ha_config)
        .block_time(block_time)
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

/// Builds the HA-sequencer full stack every proof-system test runs against.
///
/// `Ok(None)` means Docker is unavailable — the caller should skip, not fail.
pub async fn try_build_ha_devnet(skip_label: &str) -> eyre::Result<Option<WorldDevnet>> {
    try_build_ha_devnet_with_custom_block_time(skip_label, Duration::from_secs(1)).await
}

pub(in crate::it) fn l1_rpc_url(devnet: &WorldDevnet) -> eyre::Result<&str> {
    devnet
        .l1_rpc_url()
        .ok_or_eyre("full-stack devnet missing L1 RPC")
}

pub fn l2_op_node_rpc_url(devnet: &WorldDevnet) -> eyre::Result<&str> {
    devnet
        .l2_op_node_rpc_url()
        .ok_or_eyre("full-stack devnet missing L2 op-node RPC")
}

/// Parses one of the devnet's optional L1 contract addresses, naming it if absent.
pub(in crate::it) fn l1_contract(address: Option<&str>, what: &str) -> eyre::Result<Address> {
    Ok(address
        .ok_or_else(|| eyre!("full-stack devnet missing {what}"))?
        .parse()?)
}

/// Builds an HTTP provider that signs with `signer`.
///
/// Not erased to `DynProvider`: [`AlloyProofSystemClient`]'s proposer traits need
/// [`WalletProvider`], which erasure drops.
pub(in crate::it) fn signing_provider(
    rpc: &str,
    signer: PrivateKeySigner,
) -> eyre::Result<impl Provider + WalletProvider + Clone + use<>> {
    Ok(ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(rpc)?))
}

/// Funds the address originated from the provided private key with ETH and returns the related provider.
pub async fn fund_address(
    l1_rpc: &str,
    private_key: &str,
) -> eyre::Result<impl Provider + WalletProvider + Clone + use<>> {
    let signer = PrivateKeySigner::from_str(private_key)?;
    let address = signer.address();
    ProviderBuilder::new()
        .connect_http(Url::parse(l1_rpc)?)
        .anvil_set_balance(address, U256::from(THROWAWAY_ACCOUNT_BALANCE_WEI))
        .await?;

    let provider = signing_provider(l1_rpc, signer)?;
    Ok(provider)
}

/// Funds a fresh random account with L1 gas and mock tokens deposited into the active bond vault.
///
/// Cheating balance in leaves the shared devnet genesis untouched for every other test.
pub(in crate::it) async fn funded_throwaway_provider(
    l1_rpc: &str,
    factory_address: Address,
) -> eyre::Result<(Address, impl Provider + WalletProvider + Clone + use<>)> {
    let signer = PrivateKeySigner::random();
    let address = signer.address();
    ProviderBuilder::new()
        .connect_http(Url::parse(l1_rpc)?)
        .anvil_set_balance(address, U256::from(THROWAWAY_ACCOUNT_BALANCE_WEI))
        .await?;

    let provider = signing_provider(l1_rpc, signer)?;
    let factory =
        IDisputeGameFactory::IDisputeGameFactoryInstance::new(factory_address, provider.clone());
    let vault_address = read_registered_bond_vault(&provider, &factory).await?;
    let vault =
        IERC20StakingVault::IERC20StakingVaultInstance::new(vault_address, provider.clone());
    let bond_token_address = vault.token().call().await?;
    let mock_bond_token = IMockBondToken::new(bond_token_address, provider.clone());
    let amount = U256::from(THROWAWAY_ACCOUNT_BOND_TOKEN_BALANCE);

    ensure!(
        mock_bond_token
            .mint(address, amount)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "minting throwaway mock bond tokens reverted"
    );
    ensure!(
        mock_bond_token
            .approve(vault_address, amount)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "approving throwaway mock bond tokens reverted"
    );
    ensure!(
        vault
            .deposit(address, amount)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "depositing throwaway mock bond tokens reverted"
    );
    ensure!(
        mock_bond_token
            .allowance(address, vault_address)
            .call()
            .await?
            .is_zero(),
        "deposit left a bond-token allowance for the throwaway account"
    );

    Ok((address, provider))
}

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

pub(in crate::it) fn game_at<P>(
    address: Address,
    provider: P,
) -> IMultiProofGame::IMultiProofGameInstance<P>
where
    P: Provider,
{
    IMultiProofGame::IMultiProofGameInstance::new(address, provider)
}

pub(in crate::it) fn anchor_at<P>(
    address: Address,
    provider: P,
) -> IAnchorStateRegistry::IAnchorStateRegistryInstance<P>
where
    P: Provider,
{
    IAnchorStateRegistry::IAnchorStateRegistryInstance::new(address, provider)
}

pub(in crate::it) fn vault_at<P>(
    address: Address,
    provider: P,
) -> IERC20StakingVault::IERC20StakingVaultInstance<P>
where
    P: Provider,
{
    IERC20StakingVault::IERC20StakingVaultInstance::new(address, provider)
}

/// Polls `probe` until it yields a value, giving up after [`GAME_WAIT_TIMEOUT`].
///
/// `Ok(None)` means keep waiting; an `Err` aborts immediately, which is how callers fail fast on
/// terminal states. `what` completes "timed out after 300s waiting for …".
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

/// Warps Anvil's clock forward to `target` and mines a block so it is observable on-chain.
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

/// Waits for the newest WIP-1006 game at or beyond `min_l2_block`, returning its factory index,
/// address, and L2 sequence number. Pass `0` for any game at all.
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

/// Waits for a WIP-1006 game matching `parent_ref`, `l2_block`, and `attempt`.
pub async fn wait_for_multi_proof_game_attempt<P>(
    provider: P,
    factory_address: Address,
    parent_ref: Address,
    l2_block: u64,
    attempt: u64,
) -> eyre::Result<Address>
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
            let game_l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
            let game_attempt: u64 = game.attempt().call().await?.try_into()?;
            let game_parent = game.parentRef().call().await?;
            if game_l2_block == l2_block && game_attempt == attempt && game_parent == parent_ref {
                return Ok(entry.proxy);
            }
        }

        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!(
                "timed out waiting for WIP-1006 game parent={parent_ref} l2={l2_block} attempt={attempt}"
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
/// Needed where no service is watching the game; it would sit `InProgress` forever otherwise. The
/// final status must still be `expected`.
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

/// Calls `resolve()` unless another actor already did; only the final outcome matters.
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

/// Waits for the complete bond pot of `game_address` to be settled into reusable vault balances.
pub(in crate::it) async fn wait_for_game_settlement<P>(
    vault: &IERC20StakingVault::IERC20StakingVaultInstance<P>,
    game_address: Address,
) -> eyre::Result<()>
where
    P: Provider,
{
    poll_until("game bond settlement", || async {
        Ok(vault
            .gameBonds(game_address)
            .call()
            .await?
            .settled
            .then_some(()))
    })
    .await
}
