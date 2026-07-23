use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolValue, sol};
use eyre::eyre::{Context, OptionExt, bail, ensure};
use serde_json::Value;
use url::Url;
use world_chain_devnet::{
    HaSequencerConfig, ObservabilityConfig, WorldDevnetBuilder, WorldDevnetPreset,
    is_docker_unavailable,
};
use world_chain_proofs::{
    IWorldChainAnchorStateRegistry, IWorldChainProofSystemFactory, IWorldChainProofSystemGame,
};

const PROOF_SYSTEM_OWNER_KEY: &str =
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
const CHALLENGE_PERIOD_SECONDS: u64 = 24 * 60 * 60;
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");
const GAME_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
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
        error OptimismPortal_ImproperDisputeGame();
        error OptimismPortal_InvalidRootClaim();

        function anchorStateRegistry() external view returns (address);
        function disputeGameFactory() external view returns (address);
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
#[tokio::test(flavor = "multi_thread")]
async fn wip_1006_portal_withdrawal_happy_and_rejection_paths() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ha_config = HaSequencerConfig::default()
        .with_sequencer_count(3)
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
        .proof_system_factory()
        .ok_or_eyre("full-stack devnet missing WIP-1006 factory")?
        .parse()?;
    let anchor_address: Address = devnet
        .anchor_state_registry()
        .ok_or_eyre("full-stack devnet missing WIP-1006 registry")?
        .parse()?;

    let signer: PrivateKeySigner = PROOF_SYSTEM_OWNER_KEY.parse()?;
    let withdrawal_sender = signer.address();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_http(Url::parse(l1_rpc)?);
    let l2_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(&devnet.l2_rpc_url())?);

    let portal = OptimismPortal::new(portal_address, l1_provider.clone());
    ensure!(
        portal.version().call().await? == "5.6.1",
        "devnet Portal version does not match the pinned compatibility target"
    );
    ensure!(
        portal.anchorStateRegistry().call().await? == anchor_address,
        "Portal is not wired to the WIP-1006 registry"
    );
    ensure!(
        portal.disputeGameFactory().call().await? == factory_address,
        "Portal is not wired to the WIP-1006 factory"
    );

    let withdrawal = initiate_withdrawal(l2_provider.clone(), withdrawal_sender).await?;
    let (game_index, game_address, game_l2_block) =
        wait_for_covering_game(l1_provider.clone(), factory_address, withdrawal.l2_block).await?;
    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, withdrawal.hash).await?;

    let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
        game_address,
        l1_provider.clone(),
    );
    let anchor = IWorldChainAnchorStateRegistry::IWorldChainAnchorStateRegistryInstance::new(
        anchor_address,
        l1_provider.clone(),
    );
    let blacklist_receipt = anchor
        .setGameBlacklisted(game_address, true)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(
        blacklist_receipt.status(),
        "blacklisting the in-progress game reverted"
    );
    let Err(blacklisted_error) = portal
        .proveWithdrawalTransaction(
            withdrawal.transaction.clone(),
            U256::from(game_index),
            output_root_proof.clone(),
            withdrawal_proof.clone(),
        )
        .call()
        .await
    else {
        bail!("Portal accepted a withdrawal proof against a blacklisted WIP-1006 game");
    };
    ensure!(
        blacklisted_error
            .as_decoded_error::<OptimismPortal::OptimismPortal_ImproperDisputeGame>()
            .is_some(),
        "Portal accepted a withdrawal proof against a blacklisted WIP-1006 game"
    );
    ensure!(
        anchor
            .setGameBlacklisted(game_address, false)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "unblacklisting the WIP-1006 game reverted"
    );

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
    let maturity_delay: u64 = portal
        .proofMaturityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    advance_time(
        &l1_provider,
        maturity_delay.max(CHALLENGE_PERIOD_SECONDS) + 1,
    )
    .await?;

    let Err(unresolved_error) = portal
        .finalizeWithdrawalTransaction(withdrawal.transaction.clone())
        .call()
        .await
    else {
        bail!("Portal finalized a withdrawal against an unresolved WIP-1006 game");
    };
    ensure!(
        unresolved_error
            .as_decoded_error::<OptimismPortal::OptimismPortal_InvalidRootClaim>()
            .is_some(),
        "Portal did not reject the unresolved WIP-1006 root claim"
    );

    resolve_games_through(
        l1_provider.clone(),
        factory_address,
        withdrawal_sender,
        game_index,
    )
    .await?;
    ensure!(
        game.status().call().await? == GAME_DEFENDER_WINS,
        "withdrawal game did not resolve defender-win"
    );

    advance_time(&l1_provider, 1).await?;

    ensure!(
        portal
            .finalizeWithdrawalTransaction(withdrawal.transaction)
            .nonce(
                l1_provider
                    .get_transaction_count(withdrawal_sender)
                    .pending()
                    .await?,
            )
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
    let factory = IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance::new(
        factory_address,
        provider.clone(),
    );
    let started = Instant::now();

    loop {
        let game_count: u64 = factory.gameCount().call().await?.try_into()?;
        for index in (0..game_count).rev() {
            let game_address = factory.gameAtIndex(U256::from(index)).call().await?.game;
            let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
                game_address,
                provider.clone(),
            );
            let l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
            if l2_block >= withdrawal_block {
                return Ok((index, game_address, l2_block));
            }
        }

        if started.elapsed() >= GAME_WAIT_TIMEOUT {
            bail!(
                "timed out waiting for a WIP-1006 game covering withdrawal block {withdrawal_block}"
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

    let output_root_proof = OutputRootProof {
        version: B256::ZERO,
        stateRoot: block.header.state_root(),
        messagePasserStorageRoot: account_proof.storage_hash,
        latestBlockhash: block.header.hash,
    };
    Ok((output_root_proof, storage_proof.proof.clone()))
}

async fn resolve_games_through<P>(
    provider: P,
    factory_address: Address,
    sender: Address,
    game_index: u64,
) -> eyre::Result<()>
where
    P: Provider + Clone,
{
    let factory = IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance::new(
        factory_address,
        provider.clone(),
    );

    for index in 0..=game_index {
        let game_address = factory.gameAtIndex(U256::from(index)).call().await?.game;
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game_address,
            provider.clone(),
        );
        if game.status().call().await? != GAME_IN_PROGRESS {
            continue;
        }

        let nonce = provider.get_transaction_count(sender).pending().await?;
        match game.resolve().nonce(nonce).send().await {
            Ok(pending) => ensure!(
                pending.get_receipt().await?.status(),
                "game resolution reverted"
            ),
            Err(error) => ensure!(
                game.status().call().await? == GAME_DEFENDER_WINS,
                "failed to resolve game {game_address}: {error}"
            ),
        }
    }

    Ok(())
}

async fn advance_time<P>(provider: &P, seconds: u64) -> eyre::Result<()>
where
    P: Provider,
{
    let _: Value = provider
        .raw_request(Cow::Borrowed("evm_increaseTime"), (seconds,))
        .await
        .context("evm_increaseTime failed")?;
    let _: Value = provider
        .raw_request(Cow::Borrowed("evm_mine"), ())
        .await
        .context("evm_mine failed")?;
    Ok(())
}
