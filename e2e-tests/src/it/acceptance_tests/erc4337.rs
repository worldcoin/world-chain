use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{
    Address, B256, Bytes, FixedBytes, U256, address,
    aliases::{U48, U192},
    fixed_bytes, keccak256,
};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_types_eth::{
    TransactionInput, TransactionRequest,
    erc4337::{PackedUserOperation, UserOperationReceipt},
};
use alloy_signer::SignerSync;
use alloy_sol_types::{SolCall, SolValue, sol};
use eyre::eyre::{Context, OptionExt, bail, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use serde::Serialize;
use tokio::time::{Instant, sleep, timeout};
use tracing::{info, warn};
use world_chain_test_utils::utils::signer;

use super::{config::BundlerConfig, rpc::RpcEnv};

const SAFE_OP_TYPE_HASH: FixedBytes<32> =
    fixed_bytes!("c03dfc11d8b10bf9cf703d558958c8c42777f785d998c62060d85a4f0ef6ea7f");
const SAFE_DOMAIN_TYPE_HASH: FixedBytes<32> =
    fixed_bytes!("47e79534a245952e8b16893a336b85a3d9ea9fa8c573f3d803afb92a79469218");
const PARALLEL_NOOP_NONCE_KEY: u64 = 10;
const MULTI_LANE_NONCE_KEYS: [u64; 3] = [20, 21, 22];
const SINGLE_WALLET_NONCE_OPS: u64 = 4;
const SAD_PATH_REJECTION_CHECKS: u64 = 7;

sol! {
    struct EncodedSafeOpStruct {
        bytes32 typeHash;
        address safe;
        uint256 nonce;
        bytes32 initCodeHash;
        bytes32 callDataHash;
        uint128 verificationGasLimit;
        uint128 callGasLimit;
        uint256 preVerificationGas;
        uint128 maxPriorityFeePerGas;
        uint128 maxFeePerGas;
        bytes32 paymasterAndDataHash;
        uint48 validAfter;
        uint48 validUntil;
        address entryPoint;
    }

    contract Safe4337WalletDeployer {
        function getWalletAddress(address owner, uint256 saltNonce) external view returns (address wallet);
        function deployWallet(address owner, uint256 saltNonce) external returns (address wallet);
    }

    contract Safe4337Module {
        function executeUserOp(address to, uint256 value, bytes data, uint8 operation) external;
    }

    contract EntryPointNonce {
        function getNonce(address sender, uint192 key) external view returns (uint256 nonce);
    }
}

pub(super) async fn sponsored_user_operations(env: &RpcEnv) -> eyre::Result<()> {
    let Some(config) = env.config().bundler.as_ref() else {
        info!("ACCEPTANCE_BUNDLER_RPC_URL not set, skipping ERC-4337 acceptance tests");
        return Ok(());
    };
    let bundler = env
        .bundler_provider()
        .ok_or_eyre("ERC-4337 bundler provider missing")?;

    let harness = UserOperationHarness::new(
        env.chain_provider(),
        bundler,
        config,
        env.config().expected_chain_id,
    )?;

    info!(
        network = env.config().network,
        chain_rpc = env.config().rpc_target(),
        bundler_rpc = config.rpc_target(),
        entry_point = ?config.entry_point,
        wallets = config.wallet_count,
        deploy_concurrency = config.deploy_concurrency,
        operations_per_wallet = config.operations_per_wallet,
        operation_concurrency = config.operation_concurrency,
        "running ERC-4337 sponsored user operation acceptance tests"
    );

    harness.run().await
}

struct UserOperationHarness<'a> {
    chain: &'a RootProvider,
    bundler: &'a RootProvider,
    config: &'a BundlerConfig,
    chain_id: u64,
    salt_base: U256,
}

#[derive(Clone)]
struct TestWallet {
    owner_index: u32,
    owner: Address,
    sender: Address,
    salt: U256,
}

#[derive(Default)]
struct UserOperationOverrides {
    pre_verification_gas: U256,
    max_fee_per_gas: U256,
    max_priority_fee_per_gas: U256,
    paymaster: Option<Address>,
    invalidate_signature: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserOperationPermissions {
    bundler_sponsorship: BundlerSponsorship,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundlerSponsorship {
    max_cost: String,
    valid_until: u64,
}

impl<'a> UserOperationHarness<'a> {
    fn new(
        chain: &'a RootProvider,
        bundler: &'a RootProvider,
        config: &'a BundlerConfig,
        chain_id: u64,
    ) -> eyre::Result<Self> {
        ensure!(
            config.wallet_count > 0,
            "ACCEPTANCE_4337_WALLET_COUNT must be greater than zero"
        );
        ensure!(
            config.deploy_concurrency > 0,
            "ACCEPTANCE_4337_DEPLOY_CONCURRENCY must be greater than zero"
        );
        ensure!(
            config.operations_per_wallet > 0,
            "ACCEPTANCE_4337_OPS_PER_WALLET must be greater than zero"
        );
        ensure!(
            config.operation_concurrency > 0,
            "ACCEPTANCE_4337_OP_CONCURRENCY must be greater than zero"
        );
        ensure!(
            config.nonce_concurrency > 0,
            "ACCEPTANCE_4337_NONCE_CONCURRENCY must be greater than zero"
        );

        Ok(Self {
            chain,
            bundler,
            config,
            chain_id,
            salt_base: unique_salt_base()?,
        })
    }

    async fn run(&self) -> eyre::Result<()> {
        eprintln!("erc4337: checking bundler-supported entry points");
        self.assert_supported_entry_point().await?;

        eprintln!(
            "erc4337: deriving {} ephemeral Safe wallets",
            self.config.wallet_count
        );
        let wallets = stream::iter(0..self.config.wallet_count)
            .map(Ok)
            .and_then(|offset| self.fresh_wallet(offset))
            .try_collect::<Vec<_>>()
            .await?;

        eprintln!(
            "erc4337: deploying {} wallets with concurrency {}",
            wallets.len(),
            self.config.deploy_concurrency
        );
        stream::iter(wallets.clone())
            .map(Ok)
            .try_for_each_concurrent(self.config.deploy_concurrency, |wallet| async move {
                self.send_sponsored_noop("initial wallet deployment", &wallet, 0, 0, true)
                    .await?;
                self.assert_wallet_deployed(&wallet).await
            })
            .await?;

        self.run_parallel_wallet_operation_checks(&wallets).await?;
        self.run_multi_lane_nonce_burst_checks(&wallets).await?;

        eprintln!("erc4337: running two-dimensional nonce checks");
        self.run_two_dimensional_nonce_checks(&wallets[0]).await?;
        eprintln!("erc4337: running rejection checks");
        self.run_rejection_checks(&wallets[0]).await?;

        self.log_completion_summary(wallets.len());
        Ok(())
    }

    async fn assert_supported_entry_point(&self) -> eyre::Result<()> {
        let supported: Vec<Address> = self
            .bundler
            .raw_request(Cow::Borrowed("eth_supportedEntryPoints"), ())
            .await
            .context("failed to fetch supported ERC-4337 entry points from bundler")?;

        ensure!(
            supported.contains(&self.config.entry_point),
            "bundler does not advertise configured entry point {:?}; supported={supported:?}",
            self.config.entry_point
        );

        Ok(())
    }

    async fn run_parallel_wallet_operation_checks(
        &self,
        wallets: &[TestWallet],
    ) -> eyre::Result<()> {
        for sequence in 0..self.config.operations_per_wallet {
            let expected_sequence = sequence
                .checked_add(1)
                .ok_or_eyre("parallel wallet operation sequence overflowed u64")?;

            eprintln!(
                "erc4337: running sponsored operation round {}/{} across {} wallets with concurrency {}",
                expected_sequence,
                self.config.operations_per_wallet,
                wallets.len(),
                self.config.operation_concurrency
            );

            stream::iter(wallets)
                .map(Ok)
                .try_for_each_concurrent(self.config.operation_concurrency, |wallet| async move {
                    self.assert_entry_point_nonce(wallet.sender, PARALLEL_NOOP_NONCE_KEY, sequence)
                        .await?;
                    self.send_sponsored_noop(
                        "parallel wallet operation",
                        wallet,
                        PARALLEL_NOOP_NONCE_KEY,
                        sequence,
                        false,
                    )
                    .await?;
                    self.assert_entry_point_nonce(
                        wallet.sender,
                        PARALLEL_NOOP_NONCE_KEY,
                        expected_sequence,
                    )
                    .await
                })
                .await?;
        }

        Ok(())
    }

    async fn run_multi_lane_nonce_burst_checks(&self, wallets: &[TestWallet]) -> eyre::Result<()> {
        let operation_count = wallets.len() * MULTI_LANE_NONCE_KEYS.len();
        eprintln!(
            "erc4337: running multi-lane nonce burst with {operation_count} sponsored operations across {} wallets, {} nonce keys, concurrency {}",
            wallets.len(),
            MULTI_LANE_NONCE_KEYS.len(),
            self.config.operation_concurrency
        );

        let operations = wallets.iter().flat_map(|wallet| {
            MULTI_LANE_NONCE_KEYS
                .into_iter()
                .map(move |nonce_key| (wallet, nonce_key))
        });

        stream::iter(operations)
            .map(Ok)
            .try_for_each_concurrent(
                self.config.operation_concurrency,
                |(wallet, nonce_key)| async move {
                    self.assert_entry_point_nonce(wallet.sender, nonce_key, 0)
                        .await?;
                    self.send_sponsored_noop("multi-lane nonce burst", wallet, nonce_key, 0, false)
                        .await?;
                    self.assert_entry_point_nonce(wallet.sender, nonce_key, 1)
                        .await
                },
            )
            .await
    }

    async fn run_two_dimensional_nonce_checks(&self, wallet: &TestWallet) -> eyre::Result<()> {
        self.assert_entry_point_nonce(wallet.sender, 1, 0).await?;
        self.send_sponsored_noop("2D nonce key 1", wallet, 1, 0, false)
            .await?;
        self.assert_entry_point_nonce(wallet.sender, 1, 1).await?;

        self.send_sponsored_noop("2D nonce key 2", wallet, 2, 0, false)
            .await?;
        self.assert_entry_point_nonce(wallet.sender, 1, 1).await?;
        self.assert_entry_point_nonce(wallet.sender, 2, 1).await?;

        stream::iter([3_u64, 4])
            .map(Ok)
            .try_for_each_concurrent(self.config.nonce_concurrency, |key| async move {
                self.send_sponsored_noop("concurrent 2D nonce key", wallet, key, 0, false)
                    .await?;
                self.assert_entry_point_nonce(wallet.sender, key, 1).await
            })
            .await
    }

    async fn run_rejection_checks(&self, deployed_wallet: &TestWallet) -> eyre::Result<()> {
        let replay = self
            .signed_noop_user_operation(
                deployed_wallet,
                1,
                0,
                false,
                UserOperationOverrides::default(),
            )
            .await?;
        self.expect_send_rejected("replayed 2D nonce", replay, true)
            .await?;

        let gap = self
            .signed_noop_user_operation(
                deployed_wallet,
                9,
                2,
                false,
                UserOperationOverrides::default(),
            )
            .await?;
        self.expect_send_rejected("2D nonce sequence gap", gap, true)
            .await?;

        let without_permissions = self.fresh_wallet(self.config.wallet_count).await?;
        let zero_fee_op = self
            .signed_noop_user_operation(
                &without_permissions,
                0,
                0,
                true,
                UserOperationOverrides::default(),
            )
            .await?;
        self.expect_send_rejected(
            "zero fee operation without sponsorship permissions",
            zero_fee_op,
            false,
        )
        .await?;

        let nonzero_fee_wallet = self.fresh_wallet(self.config.wallet_count + 1).await?;
        let nonzero_fee_op = self
            .signed_noop_user_operation(
                &nonzero_fee_wallet,
                0,
                0,
                true,
                UserOperationOverrides {
                    max_fee_per_gas: U256::from(1),
                    max_priority_fee_per_gas: U256::from(1),
                    ..Default::default()
                },
            )
            .await?;
        self.expect_send_rejected(
            "sponsored operation with non-zero fee fields",
            nonzero_fee_op,
            true,
        )
        .await?;

        let nonzero_pvg_wallet = self.fresh_wallet(self.config.wallet_count + 2).await?;
        let nonzero_pvg_op = self
            .signed_noop_user_operation(
                &nonzero_pvg_wallet,
                0,
                0,
                true,
                UserOperationOverrides {
                    pre_verification_gas: U256::from(1),
                    ..Default::default()
                },
            )
            .await?;
        self.expect_send_rejected(
            "sponsored operation with non-zero preVerificationGas",
            nonzero_pvg_op,
            true,
        )
        .await?;

        let paymaster_wallet = self.fresh_wallet(self.config.wallet_count + 3).await?;
        let paymaster_op = self
            .signed_noop_user_operation(
                &paymaster_wallet,
                0,
                0,
                true,
                UserOperationOverrides {
                    paymaster: Some(address!("000000000000000000000000000000000000dEaD")),
                    ..Default::default()
                },
            )
            .await?;
        self.expect_send_rejected("sponsored operation with paymaster", paymaster_op, true)
            .await?;

        let bad_sig_wallet = self.fresh_wallet(self.config.wallet_count + 4).await?;
        let bad_sig_op = self
            .signed_noop_user_operation(
                &bad_sig_wallet,
                0,
                0,
                true,
                UserOperationOverrides {
                    invalidate_signature: true,
                    ..Default::default()
                },
            )
            .await?;
        self.expect_send_rejected("invalid Safe signature", bad_sig_op, true)
            .await
    }

    async fn send_sponsored_noop(
        &self,
        label: &'static str,
        wallet: &TestWallet,
        nonce_key: u64,
        sequence: u64,
        include_factory: bool,
    ) -> eyre::Result<UserOperationReceipt> {
        log_user_operation_attempt(label, wallet, nonce_key, sequence, include_factory);

        let user_op = self
            .signed_noop_user_operation(
                wallet,
                nonce_key,
                sequence,
                include_factory,
                UserOperationOverrides::default(),
            )
            .await?;

        let hash = self
            .send_sponsored_user_operation(user_op)
            .await
            .with_context(|| {
                format!(
                    "failed during {label}: owner_index={}, owner={:?}, sender={:?}, nonce_key={nonce_key}, sequence={sequence}, include_factory={include_factory}",
                    wallet.owner_index, wallet.owner, wallet.sender
                )
            })?;
        let receipt = self
            .wait_for_receipt(hash)
            .await
            .with_context(|| {
                format!(
                    "user operation {hash:?} was not included during {label}: owner_index={}, sender={:?}, nonce_key={nonce_key}, sequence={sequence}, include_factory={include_factory}",
                    wallet.owner_index, wallet.sender
                )
            })?;

        ensure!(
            receipt.success,
            "user operation {hash:?} failed in EntryPoint"
        );
        ensure!(
            receipt.sender == wallet.sender,
            "user operation receipt sender mismatch: expected {:?}, got {:?}",
            wallet.sender,
            receipt.sender
        );
        ensure!(
            receipt.entry_point == self.config.entry_point,
            "user operation receipt entry point mismatch: expected {:?}, got {:?}",
            self.config.entry_point,
            receipt.entry_point
        );
        ensure!(
            receipt.paymaster == Address::ZERO,
            "sponsored user operation unexpectedly used paymaster {:?}",
            receipt.paymaster
        );
        ensure!(
            receipt.actual_gas_cost == U256::ZERO,
            "sponsored user operation charged account gas cost {}",
            receipt.actual_gas_cost
        );

        Ok(receipt)
    }

    async fn signed_noop_user_operation(
        &self,
        wallet: &TestWallet,
        nonce_key: u64,
        sequence: u64,
        include_factory: bool,
        overrides: UserOperationOverrides,
    ) -> eyre::Result<PackedUserOperation> {
        let mut user_op = PackedUserOperation {
            sender: wallet.sender,
            nonce: packed_nonce(nonce_key, sequence),
            factory: include_factory.then_some(self.config.wallet_deployer),
            factory_data: include_factory.then(|| deploy_wallet_data(wallet.owner, wallet.salt)),
            call_data: execute_noop_data(),
            call_gas_limit: U256::from(100_000),
            verification_gas_limit: if include_factory {
                U256::from(2_000_000)
            } else {
                U256::from(500_000)
            },
            pre_verification_gas: overrides.pre_verification_gas,
            max_fee_per_gas: overrides.max_fee_per_gas,
            max_priority_fee_per_gas: overrides.max_priority_fee_per_gas,
            paymaster: overrides.paymaster,
            paymaster_verification_gas_limit: overrides.paymaster.map(|_| U256::from(100_000)),
            paymaster_post_op_gas_limit: overrides.paymaster.map(|_| U256::from(100_000)),
            paymaster_data: overrides.paymaster.map(|_| Bytes::new()),
            signature: Bytes::new(),
        };

        let hash = safe_operation_hash(
            &user_op,
            self.config.module,
            self.config.entry_point,
            self.chain_id,
        )?;
        let signature = signer(wallet.owner_index)
            .sign_message_sync(&hash.0)
            .context("failed to sign Safe user operation hash")?;
        let mut signature_bytes = Vec::new();
        let v: FixedBytes<1> = if signature.v() as u8 == 0 {
            fixed_bytes!("1F")
        } else {
            fixed_bytes!("20")
        };
        signature_bytes.extend_from_slice(
            &(
                fixed_bytes!("000000000000000000000000"),
                signature.r(),
                signature.s(),
                v,
            )
                .abi_encode_packed(),
        );
        if overrides.invalidate_signature {
            let last = signature_bytes
                .last_mut()
                .ok_or_eyre("generated Safe signature was empty")?;
            *last ^= 1;
        }
        user_op.signature = signature_bytes.into();

        Ok(user_op)
    }

    async fn send_sponsored_user_operation(
        &self,
        user_op: PackedUserOperation,
    ) -> eyre::Result<B256> {
        let hash = self
            .bundler
            .raw_request(
                Cow::Borrowed("eth_sendUserOperation"),
                (user_op, self.config.entry_point, self.permissions()?),
            )
            .await
            .context("failed to send sponsored user operation")?;

        Ok(hash)
    }

    async fn send_user_operation_without_permissions(
        &self,
        user_op: PackedUserOperation,
    ) -> eyre::Result<B256> {
        let hash = self
            .bundler
            .raw_request(
                Cow::Borrowed("eth_sendUserOperation"),
                (user_op, self.config.entry_point),
            )
            .await
            .context("failed to send user operation without permissions")?;

        Ok(hash)
    }

    async fn expect_send_rejected(
        &self,
        label: &'static str,
        user_op: PackedUserOperation,
        with_permissions: bool,
    ) -> eyre::Result<()> {
        let send = async {
            if with_permissions {
                self.send_sponsored_user_operation(user_op).await
            } else {
                self.send_user_operation_without_permissions(user_op).await
            }
        };

        match timeout(self.config.user_operation_reject_timeout, send).await {
            Ok(Ok(hash)) => bail!("{label} was accepted by bundler as {hash:?}"),
            Ok(Err(error)) => {
                info!(%label, %error, "user operation rejected as expected");
                Ok(())
            }
            Err(_) => bail!(
                "{label} did not reject within {:?}",
                self.config.user_operation_reject_timeout
            ),
        }
    }

    async fn wait_for_receipt(&self, hash: B256) -> eyre::Result<UserOperationReceipt> {
        let deadline = Instant::now() + self.config.user_operation_timeout;
        let mut last_error = None;

        loop {
            let result: Result<Option<UserOperationReceipt>, _> = self
                .bundler
                .raw_request(Cow::Borrowed("eth_getUserOperationReceipt"), (hash,))
                .await;

            match result {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => {}
                Err(error) => {
                    warn!(%hash, %error, "failed to fetch user operation receipt, retrying");
                    last_error = Some(error.to_string());
                }
            }

            let now = Instant::now();
            if now >= deadline {
                if let Some(error) = last_error {
                    bail!(
                        "timed out after {:?} waiting for user operation {hash:?}; last receipt fetch error: {error}",
                        self.config.user_operation_timeout
                    );
                }

                bail!(
                    "timed out after {:?} waiting for user operation {hash:?}",
                    self.config.user_operation_timeout
                );
            }

            let remaining = deadline.saturating_duration_since(now);
            sleep(self.config.user_operation_poll_interval.min(remaining)).await;
        }
    }

    async fn fresh_wallet(&self, offset: usize) -> eyre::Result<TestWallet> {
        let owner_index_offset = u32::try_from(offset)
            .wrap_err("ACCEPTANCE_4337_WALLET_COUNT exceeds u32 owner index space")?;
        let owner_index = self
            .config
            .owner_start_index
            .checked_add(owner_index_offset)
            .ok_or_eyre("derived Safe owner index overflowed u32")?;
        let owner = signer(owner_index).address();
        let salt = self.salt_base + U256::from(offset);
        let sender = self.wallet_address(owner, salt).await?;

        Ok(TestWallet {
            owner_index,
            owner,
            sender,
            salt,
        })
    }

    async fn wallet_address(&self, owner: Address, salt: U256) -> eyre::Result<Address> {
        let call = Safe4337WalletDeployer::getWalletAddressCall {
            owner,
            saltNonce: salt,
        };
        let output = self
            .eth_call(self.config.wallet_deployer, call.abi_encode().into())
            .await
            .context("failed to call Safe4337WalletDeployer.getWalletAddress")?;

        Safe4337WalletDeployer::getWalletAddressCall::abi_decode_returns(&output)
            .context("failed to decode Safe4337WalletDeployer.getWalletAddress return")
    }

    async fn assert_wallet_deployed(&self, wallet: &TestWallet) -> eyre::Result<()> {
        let code = self
            .chain
            .get_code_at(wallet.sender)
            .await
            .with_context(|| format!("failed to fetch code for Safe wallet {:?}", wallet.sender))?;

        ensure!(
            !code.is_empty(),
            "Safe wallet {:?} was not deployed",
            wallet.sender
        );

        Ok(())
    }

    async fn assert_entry_point_nonce(
        &self,
        sender: Address,
        key: u64,
        expected_sequence: u64,
    ) -> eyre::Result<()> {
        let actual = self.entry_point_nonce(sender, key).await?;
        let expected = packed_nonce(key, expected_sequence);

        ensure!(
            actual == expected,
            "EntryPoint nonce mismatch for {sender:?} key {key}: expected {expected}, got {actual}"
        );

        Ok(())
    }

    async fn entry_point_nonce(&self, sender: Address, key: u64) -> eyre::Result<U256> {
        let call = EntryPointNonce::getNonceCall {
            sender,
            key: U192::from(key),
        };
        let output = self
            .eth_call(self.config.entry_point, call.abi_encode().into())
            .await
            .context("failed to call EntryPoint.getNonce")?;

        EntryPointNonce::getNonceCall::abi_decode_returns(&output)
            .context("failed to decode EntryPoint.getNonce return")
    }

    async fn eth_call(&self, to: Address, input: Bytes) -> eyre::Result<Bytes> {
        let request = TransactionRequest::default()
            .to(to)
            .input(TransactionInput::new(input));

        self.chain
            .raw_request(Cow::Borrowed("eth_call"), (request, "latest"))
            .await
            .context("eth_call failed")
    }

    fn permissions(&self) -> eyre::Result<UserOperationPermissions> {
        let valid_until = SystemTime::now()
            .checked_add(self.config.sponsorship_validity)
            .ok_or_eyre("sponsorship validUntil overflowed SystemTime")?
            .duration_since(UNIX_EPOCH)
            .wrap_err("system clock is before UNIX_EPOCH")?
            .as_secs();

        Ok(UserOperationPermissions {
            bundler_sponsorship: BundlerSponsorship {
                max_cost: format!("{:#x}", self.config.sponsorship_max_cost),
                valid_until,
            },
        })
    }

    fn log_completion_summary(&self, wallet_count: usize) {
        let wallet_count = wallet_count as u128;
        let deploy_ops = wallet_count;
        let parallel_ops = wallet_count * u128::from(self.config.operations_per_wallet);
        let multi_lane_nonce_ops = wallet_count * MULTI_LANE_NONCE_KEYS.len() as u128;
        let successful_user_ops =
            deploy_ops + parallel_ops + multi_lane_nonce_ops + u128::from(SINGLE_WALLET_NONCE_OPS);

        eprintln!(
            "erc4337: completed sponsored checks: wallets={wallet_count}, successful_user_ops={successful_user_ops}, deploy_ops={deploy_ops}, parallel_ops={parallel_ops}, multi_lane_nonce_ops={multi_lane_nonce_ops}, single_wallet_nonce_ops={SINGLE_WALLET_NONCE_OPS}, sad_paths={SAD_PATH_REJECTION_CHECKS}"
        );
    }
}

fn execute_noop_data() -> Bytes {
    Safe4337Module::executeUserOpCall {
        to: Address::ZERO,
        value: U256::ZERO,
        data: Bytes::new(),
        operation: 0,
    }
    .abi_encode()
    .into()
}

fn deploy_wallet_data(owner: Address, salt: U256) -> Bytes {
    Safe4337WalletDeployer::deployWalletCall {
        owner,
        saltNonce: salt,
    }
    .abi_encode()
    .into()
}

fn packed_nonce(key: u64, sequence: u64) -> U256 {
    (U256::from(key) << 64) | U256::from(sequence)
}

fn log_user_operation_attempt(
    label: &'static str,
    wallet: &TestWallet,
    nonce_key: u64,
    sequence: u64,
    include_factory: bool,
) {
    eprintln!(
        "erc4337: sending {label}: owner_index={}, owner={:?}, sender={:?}, nonce_key={nonce_key}, sequence={sequence}, include_factory={include_factory}",
        wallet.owner_index, wallet.owner, wallet.sender
    );
}

fn safe_operation_hash(
    user_op: &PackedUserOperation,
    module: Address,
    entry_point: Address,
    chain_id: u64,
) -> eyre::Result<B256> {
    let encoded_safe_struct = EncodedSafeOpStruct {
        typeHash: SAFE_OP_TYPE_HASH,
        safe: user_op.sender,
        nonce: user_op.nonce,
        initCodeHash: keccak256(packed_factory_data(user_op)),
        callDataHash: keccak256(&user_op.call_data),
        verificationGasLimit: u256_to_u128("verificationGasLimit", user_op.verification_gas_limit)?,
        callGasLimit: u256_to_u128("callGasLimit", user_op.call_gas_limit)?,
        preVerificationGas: user_op.pre_verification_gas,
        maxPriorityFeePerGas: u256_to_u128(
            "maxPriorityFeePerGas",
            user_op.max_priority_fee_per_gas,
        )?,
        maxFeePerGas: u256_to_u128("maxFeePerGas", user_op.max_fee_per_gas)?,
        paymasterAndDataHash: keccak256(packed_paymaster_data(user_op)?),
        validAfter: U48::ZERO,
        validUntil: U48::ZERO,
        entryPoint: entry_point,
    };
    let safe_struct_hash = keccak256(encoded_safe_struct.abi_encode());
    let domain_separator = keccak256((SAFE_DOMAIN_TYPE_HASH, chain_id, module).abi_encode());
    let operation_data = (
        fixed_bytes!("19"),
        fixed_bytes!("01"),
        domain_separator,
        safe_struct_hash,
    )
        .abi_encode_packed();

    Ok(keccak256(operation_data))
}

fn packed_factory_data(user_op: &PackedUserOperation) -> Bytes {
    let Some(factory) = user_op.factory else {
        return Bytes::new();
    };
    let factory_data = user_op.factory_data.as_ref().map_or(&[][..], Bytes::as_ref);
    let mut init_code = Vec::with_capacity(20 + factory_data.len());
    init_code.extend_from_slice(factory.as_slice());
    init_code.extend_from_slice(factory_data);

    init_code.into()
}

fn packed_paymaster_data(user_op: &PackedUserOperation) -> eyre::Result<Bytes> {
    let Some(paymaster) = user_op.paymaster else {
        return Ok(Bytes::new());
    };
    let mut data = Vec::new();
    data.extend_from_slice(paymaster.as_slice());
    data.extend_from_slice(
        &u256_to_u128(
            "paymasterVerificationGasLimit",
            user_op.paymaster_verification_gas_limit.unwrap_or_default(),
        )?
        .to_be_bytes(),
    );
    data.extend_from_slice(
        &u256_to_u128(
            "paymasterPostOpGasLimit",
            user_op.paymaster_post_op_gas_limit.unwrap_or_default(),
        )?
        .to_be_bytes(),
    );
    if let Some(paymaster_data) = &user_op.paymaster_data {
        data.extend_from_slice(paymaster_data);
    }

    Ok(data.into())
}

fn u256_to_u128(name: &'static str, value: U256) -> eyre::Result<u128> {
    ensure!(
        value <= U256::from(u128::MAX),
        "{name} does not fit in uint128: {value}"
    );

    Ok(value.to())
}

fn unique_salt_base() -> eyre::Result<U256> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before UNIX_EPOCH")?
        .as_nanos();

    Ok(U256::from(nanos) << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_2d_nonce() {
        assert_eq!(packed_nonce(0, 7), U256::from(7));
        assert_eq!(packed_nonce(5, 9), (U256::from(5) << 64) | U256::from(9));
    }

    #[test]
    fn packs_factory_data_as_v0_7_init_code() {
        let user_op = PackedUserOperation {
            sender: Address::ZERO,
            nonce: U256::ZERO,
            factory: Some(address!("1111111111111111111111111111111111111111")),
            factory_data: Some(Bytes::from(vec![0xaa, 0xbb])),
            call_data: Bytes::new(),
            call_gas_limit: U256::ZERO,
            verification_gas_limit: U256::ZERO,
            pre_verification_gas: U256::ZERO,
            max_fee_per_gas: U256::ZERO,
            max_priority_fee_per_gas: U256::ZERO,
            paymaster: None,
            paymaster_verification_gas_limit: None,
            paymaster_post_op_gas_limit: None,
            paymaster_data: None,
            signature: Bytes::new(),
        };

        let packed = packed_factory_data(&user_op);
        assert_eq!(packed.len(), 22);
        assert_eq!(
            &packed[..20],
            address!("1111111111111111111111111111111111111111").as_slice()
        );
        assert_eq!(&packed[20..], &[0xaa, 0xbb]);
    }
}
