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

const PROOF_SYSTEM_OWNER_KEY: &str =
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
const CHALLENGE_PERIOD_SECONDS: u64 = 24 * 60 * 60;
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

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

    struct GameSearchResult {
        uint256 index;
        bytes32 metadata;
        uint64 timestamp;
        bytes32 rootClaim;
        bytes extraData;
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
        error OptimismPortal_ProofNotOldEnough();

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

    #[sol(rpc)]
    interface WorldChainProofSystemFactory {
        function gameCount() external view returns (uint256);
        function gameAtIndex(uint256 index)
            external
            view
            returns (uint32 gameType, uint64 timestamp, address game);
        function findLatestGames(uint32 gameType, uint256 start, uint256 n)
            external
            view
            returns (GameSearchResult[] games);
    }

    #[sol(rpc)]
    interface WorldChainProofSystemGame {
        error AlreadyResolved(uint8 state);

        function rootClaim() external view returns (bytes32);
        function l2SequenceNumber() external view returns (uint256);
        function status() external view returns (uint8);
        function resolve() external returns (uint8 outcome, uint8 reason);
    }

    #[sol(rpc)]
    interface WorldChainAnchorStateRegistry {
        function setGameBlacklisted(address game, bool blacklisted) external;
    }
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

    let messenger = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, l2_provider.clone());
    let withdrawal_receipt = messenger
        .initiateWithdrawal(withdrawal_sender, U256::from(100_000), Bytes::new())
        .gas(250_000)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(
        withdrawal_receipt.status(),
        "L2 withdrawal initiation reverted"
    );
    let withdrawal_block = withdrawal_receipt
        .block_number
        .ok_or_eyre("withdrawal receipt missing L2 block number")?;

    let message = withdrawal_receipt
        .logs()
        .iter()
        .find_map(|log| {
            log.log_decode_validate::<L2ToL1MessagePasser::MessagePassed>()
                .ok()
        })
        .ok_or_eyre("withdrawal receipt missing MessagePassed event")?;
    let message = message.data();
    let withdrawal = WithdrawalTransaction {
        nonce: message.nonce,
        sender: message.sender,
        target: message.target,
        value: message.value,
        gasLimit: message.gasLimit,
        data: message.data.clone(),
    };
    let withdrawal_hash = keccak256(
        (
            withdrawal.nonce,
            withdrawal.sender,
            withdrawal.target,
            withdrawal.value,
            withdrawal.gasLimit,
            withdrawal.data.clone(),
        )
            .abi_encode_params(),
    );
    ensure!(
        withdrawal_hash == message.withdrawalHash,
        "MessagePassed withdrawal hash does not match the Portal encoding"
    );

    let factory = WorldChainProofSystemFactory::new(factory_address, l1_provider.clone());
    let started = Instant::now();
    let (game_index, game_address, game_l2_block) = loop {
        let count: u64 = factory.gameCount().call().await?.try_into()?;
        let mut selected = None;
        if count != 0 {
            let games = factory
                .findLatestGames(1006, U256::from(count - 1), U256::from(count))
                .call()
                .await?;
            for result in games {
                let index: u64 = result.index.try_into()?;
                let game_at = factory.gameAtIndex(result.index).call().await?;
                ensure!(
                    game_at.game == Address::from_slice(&result.metadata.as_slice()[12..]),
                    "findLatestGames returned inconsistent packed game metadata"
                );
                ensure!(
                    game_at.timestamp == result.timestamp,
                    "findLatestGames returned an inconsistent creation timestamp"
                );

                let game = WorldChainProofSystemGame::new(game_at.game, l1_provider.clone());
                ensure!(
                    game.rootClaim().call().await? == result.rootClaim,
                    "findLatestGames returned an inconsistent root claim"
                );
                let l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
                if l2_block >= withdrawal_block {
                    selected = Some((index, game_at.game, l2_block));
                    break;
                }
            }
        }
        if let Some(selected) = selected {
            break selected;
        }
        if started.elapsed() >= Duration::from_secs(180) {
            bail!(
                "timed out waiting for a WIP-1006 game covering withdrawal block {withdrawal_block}"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

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
    let game = WorldChainProofSystemGame::new(game_address, l1_provider.clone());
    let output_root = keccak256(
        (
            output_root_proof.version,
            output_root_proof.stateRoot,
            output_root_proof.messagePasserStorageRoot,
            output_root_proof.latestBlockhash,
        )
            .abi_encode_params(),
    );
    ensure!(
        output_root == game.rootClaim().call().await?,
        "locally reconstructed output root does not match the WIP-1006 game"
    );
    let withdrawal_proof = storage_proof.proof.clone();

    let anchor = WorldChainAnchorStateRegistry::new(anchor_address, l1_provider.clone());
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
    let blacklisted_error = match portal
        .proveWithdrawalTransaction(
            withdrawal.clone(),
            U256::from(game_index),
            output_root_proof.clone(),
            withdrawal_proof.clone(),
        )
        .call()
        .await
    {
        Ok(_) => bail!("Portal accepted a withdrawal proof against a blacklisted WIP-1006 game"),
        Err(error) => error,
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
                withdrawal.clone(),
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
    let immature_error = match portal
        .finalizeWithdrawalTransaction(withdrawal.clone())
        .call()
        .await
    {
        Ok(_) => bail!("Portal finalized a withdrawal before its proof matured"),
        Err(error) => error,
    };
    ensure!(
        immature_error
            .as_decoded_error::<OptimismPortal::OptimismPortal_ProofNotOldEnough>()
            .is_some(),
        "Portal did not enforce its proof maturity delay"
    );

    let maturity_delay: u64 = portal
        .proofMaturityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    let _: Value = l1_provider
        .raw_request(
            Cow::Borrowed("evm_increaseTime"),
            (maturity_delay.max(CHALLENGE_PERIOD_SECONDS) + 1,),
        )
        .await
        .context("failed to advance beyond the proof maturity and challenge periods")?;
    let _: Value = l1_provider
        .raw_request(Cow::Borrowed("evm_mine"), ())
        .await?;

    let unresolved_error = match portal
        .finalizeWithdrawalTransaction(withdrawal.clone())
        .call()
        .await
    {
        Ok(_) => bail!("Portal finalized a withdrawal against an unresolved WIP-1006 game"),
        Err(error) => error,
    };
    ensure!(
        unresolved_error
            .as_decoded_error::<OptimismPortal::OptimismPortal_InvalidRootClaim>()
            .is_some(),
        "Portal did not reject the unresolved WIP-1006 root claim"
    );

    for index in 0..=game_index {
        let game_at = factory.gameAtIndex(U256::from(index)).call().await?;
        let ancestor = WorldChainProofSystemGame::new(game_at.game, l1_provider.clone());
        if ancestor.status().call().await? == 0 {
            let nonce = l1_provider
                .get_transaction_count(withdrawal_sender)
                .pending()
                .await?;
            match ancestor.resolve().nonce(nonce).send().await {
                Ok(pending) => ensure!(
                    pending.get_receipt().await?.status(),
                    "game resolution reverted"
                ),
                Err(error) => {
                    let finalized_by_keeper = error
                        .as_decoded_error::<WorldChainProofSystemGame::AlreadyResolved>()
                        .is_some_and(|decoded| decoded.state == 3);
                    ensure!(
                        finalized_by_keeper || ancestor.status().call().await? == 2,
                        "failed to resolve game {}: {error}",
                        game_at.game
                    );
                }
            }
        }
    }
    ensure!(
        game.status().call().await? == 2,
        "withdrawal game did not resolve defender-win"
    );

    let _: Value = l1_provider
        .raw_request(Cow::Borrowed("evm_increaseTime"), (1_u64,))
        .await
        .context("failed to advance past the ASR finality timestamp check")?;
    let _: Value = l1_provider
        .raw_request(Cow::Borrowed("evm_mine"), ())
        .await?;

    ensure!(
        portal
            .finalizeWithdrawalTransaction(withdrawal)
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
        portal.finalizedWithdrawals(withdrawal_hash).call().await?,
        "Portal did not persist the finalized withdrawal"
    );

    Ok(())
}
