use std::time::{Duration, Instant};

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_provider::{Provider, ProviderBuilder, ext::AnvilApi};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolValue, sol};
use eyre::eyre::{OptionExt, bail, ensure};
use url::Url;
use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WorldDevnetBuilder, WorldDevnetPreset,
    is_docker_unavailable,
};
use world_chain_proof_protocol::{
    IAnchorStateRegistry, IDelayedWETH, IDisputeGameFactory, IMultiProofGame, MULTI_PROOF_GAME_TYPE,
};

const DEVNET_SIGNER_KEY: &str =
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");
const GAME_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const GAME_IN_PROGRESS: u8 = 0;
const GAME_DEFENDER_WINS: u8 = 2;

sol! {
    struct WithdrawalTransaction {
        uint256 nonce;
        address sender;
        address target;
        uint256 value;
        uint256 gasLimit;
        bytes data;
    }

    struct OutputRootProof {
        bytes32 version;
        bytes32 stateRoot;
        bytes32 messagePasserStorageRoot;
        bytes32 latestBlockhash;
    }

    #[sol(rpc)]
    interface L2ToL1MessagePasser {
        event MessagePassed(
            uint256 indexed nonce,
            address indexed sender,
            address indexed target,
            uint256 value,
            uint256 gasLimit,
            bytes data,
            bytes32 withdrawalHash
        );

        function initiateWithdrawal(address target, uint256 gasLimit, bytes data) external payable;
    }

    #[sol(rpc)]
    interface OptimismPortal {
        function anchorStateRegistry() external view returns (address);
        function disputeGameFactory() external view returns (address);
        function disputeGameFinalityDelaySeconds() external view returns (uint256);
        function proofMaturityDelaySeconds() external view returns (uint256);
        function version() external view returns (string);
        function proveWithdrawalTransaction(
            WithdrawalTransaction tx_,
            uint256 disputeGameIndex,
            OutputRootProof outputRootProof,
            bytes[] withdrawalProof
        ) external;
        function finalizeWithdrawalTransaction(WithdrawalTransaction tx_) external;
        function finalizedWithdrawals(bytes32 withdrawalHash) external view returns (bool);
    }
}

struct InitiatedWithdrawal {
    transaction: WithdrawalTransaction,
    hash: B256,
    l2_block: u64,
}

#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_native_wip_1006_portal_withdrawal_and_bond_claim() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ha_config = HaSequencerConfig::default()
        .with_sequencer_count(2)
        .with_observability(ObservabilityConfig::default());
    let devnet = match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::HaSequencer)
        .ha_sequencer(ha_config)
        .block_time(Duration::from_secs(1))
        .build()
        .await
    {
        Ok(devnet) => devnet,
        Err(err) if is_docker_unavailable(&err) => {
            eprintln!("skipping Portal withdrawal E2E because Docker is unavailable: {err:#}");
            return Ok(());
        }
        Err(err) => return Err(err),
    };

    let l1_rpc = devnet
        .l1_rpc_url()
        .ok_or_eyre("full-stack devnet missing L1 RPC")?;
    let portal_address: Address = devnet
        .optimism_portal()
        .ok_or_eyre("full-stack devnet missing OptimismPortal")?
        .parse()?;
    let factory_address: Address = devnet
        .dispute_game_factory()
        .ok_or_eyre("full-stack devnet missing DisputeGameFactory")?
        .parse()?;
    let anchor_address: Address = devnet
        .anchor_state_registry()
        .ok_or_eyre("full-stack devnet missing AnchorStateRegistry")?
        .parse()?;

    let signer: PrivateKeySigner = DEVNET_SIGNER_KEY.parse()?;
    let withdrawal_sender = signer.address();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_http(Url::parse(l1_rpc)?);
    let l2_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(&devnet.l2_rpc_url())?);

    let portal = OptimismPortal::new(portal_address, l1_provider.clone());
    let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
        anchor_address,
        l1_provider.clone(),
    );
    ensure!(
        portal.version().call().await? == "5.6.1",
        "devnet Portal version does not match the pinned compatibility target"
    );
    ensure!(
        portal.anchorStateRegistry().call().await? == anchor_address,
        "Portal is not wired to the deployed AnchorStateRegistry"
    );
    ensure!(
        portal.disputeGameFactory().call().await? == factory_address,
        "Portal is not wired to the deployed DisputeGameFactory"
    );
    ensure!(
        anchor.respectedGameType().call().await? == MULTI_PROOF_GAME_TYPE,
        "WIP-1006 is not the AnchorStateRegistry's respected game type"
    );

    let withdrawal = initiate_withdrawal(l2_provider.clone(), withdrawal_sender).await?;
    let (game_index, game_address, game_l2_block) =
        wait_for_covering_game(l1_provider.clone(), factory_address, withdrawal.l2_block).await?;
    let game = IMultiProofGame::IMultiProofGameInstance::new(game_address, l1_provider.clone());
    ensure!(
        game.wasRespectedGameTypeWhenCreated().call().await?,
        "covering WIP-1006 game was not respected when created"
    );
    ensure!(
        anchor.isGameProper(game_address).call().await?,
        "covering WIP-1006 game is not proper"
    );
    wait_for_initial_proof(l1_provider.clone(), game_address).await?;

    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, withdrawal.hash).await?;
    ensure!(
        portal
            .proveWithdrawalTransaction(
                withdrawal.transaction.clone(),
                U256::from(game_index),
                output_root_proof,
                withdrawal_proof,
            )
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal proof transaction reverted"
    );

    let current_timestamp = latest_timestamp(&l1_provider).await?;
    let challenge_deadline = game.challengeDeadline().call().await?;
    let proof_maturity_delay: u64 = portal
        .proofMaturityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    advance_to_timestamp(
        &l1_provider,
        current_timestamp
            .saturating_add(proof_maturity_delay)
            .max(challenge_deadline)
            .saturating_add(1),
    )
    .await?;

    wait_for_defender_win(l1_provider.clone(), game_address).await?;
    let finality_delay: u64 = portal
        .disputeGameFinalityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    let resolved_at = game.resolvedAt().call().await?;
    advance_to_timestamp(
        &l1_provider,
        resolved_at.saturating_add(finality_delay).saturating_add(1),
    )
    .await?;
    wait_for_game_finality(l1_provider.clone(), anchor_address, game_address).await?;
    ensure!(
        anchor.isGameClaimValid(game_address).call().await?,
        "resolved WIP-1006 game is not a valid Portal claim"
    );

    ensure!(
        portal
            .finalizeWithdrawalTransaction(withdrawal.transaction)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal finalization reverted"
    );
    ensure!(
        portal.finalizedWithdrawals(withdrawal.hash).call().await?,
        "Portal did not persist the finalized withdrawal"
    );
    wait_for_anchor_at_or_beyond(l1_provider.clone(), anchor_address, game_l2_block).await?;

    let proposer = game.gameCreator().call().await?;
    let delayed_weth = game.weth().call().await?;
    let unlock_at =
        wait_for_bond_unlock(l1_provider.clone(), delayed_weth, game_address, proposer).await?;
    advance_to_timestamp(&l1_provider, unlock_at).await?;
    wait_for_bond_withdrawal(l1_provider.clone(), delayed_weth, game_address, proposer).await?;

    Ok(())
}

async fn initiate_withdrawal<P>(
    provider: P,
    withdrawal_sender: Address,
) -> eyre::Result<InitiatedWithdrawal>
where
    P: Provider,
{
    let receipt = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, provider)
        .initiateWithdrawal(withdrawal_sender, U256::from(100_000), Bytes::new())
        .gas(250_000)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(receipt.status(), "L2 withdrawal initiation reverted");

    let l2_block = receipt
        .block_number
        .ok_or_eyre("withdrawal receipt missing L2 block number")?;
    let message = receipt
        .logs()
        .iter()
        .find_map(|log| {
            log.log_decode_validate::<L2ToL1MessagePasser::MessagePassed>()
                .ok()
        })
        .ok_or_eyre("withdrawal receipt missing MessagePassed event")?;
    let message = message.data();

    Ok(InitiatedWithdrawal {
        transaction: WithdrawalTransaction {
            nonce: message.nonce,
            sender: message.sender,
            target: message.target,
            value: message.value,
            gasLimit: message.gasLimit,
            data: message.data.clone(),
        },
        hash: message.withdrawalHash,
        l2_block,
    })
}

async fn wait_for_covering_game<P>(
    provider: P,
    factory_address: Address,
    withdrawal_block: u64,
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
            let game = IMultiProofGame::IMultiProofGameInstance::new(entry.proxy, provider.clone());
            let l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
            if l2_block >= withdrawal_block {
                return Ok((index, entry.proxy, l2_block));
            }
        }

        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!(
                "timed out waiting for a respected WIP-1006 game covering withdrawal block {withdrawal_block}"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn build_withdrawal_proof<P>(
    l2_provider: P,
    game_l2_block: u64,
    withdrawal_hash: B256,
) -> eyre::Result<(OutputRootProof, Vec<Bytes>)>
where
    P: Provider,
{
    let block = l2_provider
        .get_block_by_number(BlockNumberOrTag::Number(game_l2_block))
        .await?
        .ok_or_eyre("WIP-1006 output block missing from L2")?;
    let storage_key = keccak256((withdrawal_hash, U256::ZERO).abi_encode_params());
    let account_proof = l2_provider
        .get_proof(L2_TO_L1_MESSAGE_PASSER, vec![storage_key])
        .block_id(BlockId::Number(BlockNumberOrTag::Number(game_l2_block)))
        .await?;
    let storage_proof = account_proof
        .storage_proof
        .first()
        .ok_or_eyre("eth_getProof returned no withdrawal storage proof")?;
    ensure!(
        storage_proof.value == U256::from(1),
        "withdrawal is absent from the message passer"
    );

    Ok((
        OutputRootProof {
            version: B256::ZERO,
            stateRoot: block.header.state_root(),
            messagePasserStorageRoot: account_proof.storage_hash,
            latestBlockhash: block.header.hash,
        },
        storage_proof.proof.clone(),
    ))
}

async fn wait_for_initial_proof<P>(provider: P, game_address: Address) -> eyre::Result<()>
where
    P: Provider,
{
    let game = IMultiProofGame::IMultiProofGameInstance::new(game_address, provider);
    let started = Instant::now();

    loop {
        if game.proofBitmap().call().await? != 0 {
            return Ok(());
        }
        if game.status().call().await? != GAME_IN_PROGRESS {
            bail!("game {game_address} resolved before receiving its initial proof");
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!(
                "timed out waiting for defender to submit an initial proof to game {game_address}"
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_defender_win<P>(provider: P, game_address: Address) -> eyre::Result<()>
where
    P: Provider,
{
    let game = IMultiProofGame::IMultiProofGameInstance::new(game_address, provider);
    let started = Instant::now();

    loop {
        let status = game.status().call().await?;
        if status == GAME_DEFENDER_WINS {
            return Ok(());
        }
        if status != GAME_IN_PROGRESS {
            bail!("game {game_address} resolved with unexpected status {status}");
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out waiting for game {game_address} to resolve");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_game_finality<P>(
    provider: P,
    anchor_address: Address,
    game_address: Address,
) -> eyre::Result<()>
where
    P: Provider,
{
    let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(anchor_address, provider);
    let started = Instant::now();
    loop {
        if anchor.isGameFinalized(game_address).call().await? {
            return Ok(());
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out waiting for game {game_address} to pass the ASR finality airgap");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_anchor_at_or_beyond<P>(
    provider: P,
    anchor_address: Address,
    game_l2_block: u64,
) -> eyre::Result<()>
where
    P: Provider,
{
    let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(anchor_address, provider);
    let started = Instant::now();
    loop {
        let anchor_root = anchor.getAnchorRoot().call().await?;
        if anchor_root.l2SequenceNumber >= U256::from(game_l2_block) {
            return Ok(());
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out waiting for the ASR anchor to reach L2 block {game_l2_block}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_bond_unlock<P>(
    provider: P,
    delayed_weth: Address,
    game_address: Address,
    proposer: Address,
) -> eyre::Result<u64>
where
    P: Provider + Clone,
{
    let weth = IDelayedWETH::IDelayedWETHInstance::new(delayed_weth, provider);
    let delay = weth.delay().call().await?;
    let started = Instant::now();
    loop {
        let pending = weth.withdrawals(game_address, proposer).call().await?;
        if !pending.amount.is_zero() {
            let unlock_at: u64 = pending
                .timestamp
                .checked_add(delay)
                .ok_or_eyre("DelayedWETH unlock timestamp overflow")?
                .try_into()?;
            return Ok(unlock_at);
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out waiting for proposer bond credit to unlock");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_bond_withdrawal<P>(
    provider: P,
    delayed_weth: Address,
    game_address: Address,
    proposer: Address,
) -> eyre::Result<()>
where
    P: Provider,
{
    let weth = IDelayedWETH::IDelayedWETHInstance::new(delayed_weth, provider);
    let started = Instant::now();
    loop {
        if weth
            .withdrawals(game_address, proposer)
            .call()
            .await?
            .amount
            .is_zero()
        {
            return Ok(());
        }
        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!("timed out waiting for proposer bond withdrawal");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn latest_timestamp<P>(provider: &P) -> eyre::Result<u64>
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

async fn advance_to_timestamp<P>(provider: &P, target: u64) -> eyre::Result<()>
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
