//! World Chain transaction pool types
use chrono::{DateTime, Datelike};
use reth_db::cursor::DbCursorRW;
use reth_db::transaction::{DbTx, DbTxMut};
use semaphore::hash_to_field;
use semaphore::protocol::verify_proof;
use std::str::FromStr as _;
use std::sync::Arc;
use tracing::warn;

use reth_db::{Database, DatabaseEnv, DatabaseError, DatabaseWriteOperation};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::{SealedBlock, TxHash};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    Pool, TransactionOrigin, TransactionValidationOutcome, TransactionValidationTaskExecutor,
    TransactionValidator,
};

use crate::pbh::db::{ExecutedPbhNullifierTable, ValidatedPbhTransactionTable};
use crate::pbh::semaphore::SemaphoreProof;
use crate::pbh::tx::Prefix;

use super::error::{
    TransactionValidationError, WorldChainTransactionPoolError, WorldChainTransactionPoolInvalid,
};
use super::ordering::WorldChainOrdering;
use super::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<
        WorldChainTransactionValidator<Client, WorldChainPooledTransaction>,
    >,
    WorldChainOrdering<WorldChainPooledTransaction>,
    S,
>;

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    inner: OpTransactionValidator<Client, Tx>,
    pub(crate) database_env: Arc<DatabaseEnv>,
    num_pbh_txs: u16,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WorldChainPoolTransaction,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx>,
        database_env: Arc<DatabaseEnv>,
        num_pbh_txs: u16,
    ) -> Self {
        Self {
            inner,
            database_env,
            num_pbh_txs,
        }
    }

    pub fn set_validated(
        &self,
        tx: &Tx,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), DatabaseError> {
        let db_tx = self.database_env.tx_mut()?;
        let mut cursor = db_tx.cursor_write::<ValidatedPbhTransactionTable>()?;
        cursor.insert(
            *tx.hash(),
            semaphore_proof.nullifier_hash.to_be_bytes().into(),
        )?;
        db_tx.commit()?;
        Ok(())
    }

    /// Ensure the provided root is on chain and valid
    pub fn validate_root(
        &self,
        _semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        warn!("Root validation is not implemented yet");
        Ok(())
    }

    /// External nullifiers must be of the form
    /// `<prefix>-<periodId>-<PbhNonce>`.
    /// example:
    /// `0-012025-11`
    pub fn validate_external_nullifier(
        &self,
        date: chrono::DateTime<chrono::Utc>,
        external_nullifier: &str,
    ) -> Result<(), TransactionValidationError> {
        let split = external_nullifier.split('-').collect::<Vec<&str>>();

        if split.len() != 3 {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifier.into());
        }

        // TODO: Figure out what we actually want to do with the prefix
        // For now, we just check that it's a valid prefix
        // Maybe in future use as some sort of versioning?
        if Prefix::from_str(split[0]).is_err() {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPrefix.into());
        }

        // TODO: Handle edge case where we are at the end of the month
        if split[1] != format_date(date) {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPeriod.into());
        }

        match split[2].parse::<u16>() {
            Ok(nonce) if nonce < self.num_pbh_txs => {}
            _ => {
                return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierNonce.into());
            }
        }

        Ok(())
    }

    pub fn validate_nullifier(
        &self,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let tx = self.database_env.tx().unwrap();
        match tx
            .get::<ExecutedPbhNullifierTable>(semaphore_proof.nullifier_hash.to_be_bytes().into())
        {
            Ok(Some(_)) => Err(WorldChainTransactionPoolInvalid::NullifierAlreadyExists.into()),
            Ok(None) => Ok(()),
            Err(e) => Err(TransactionValidationError::Error(
                format!("Error while fetching nullifier from database: {}", e).into(),
            )),
        }
    }

    pub fn validate_signal_hash(
        &self,
        tx_hash: &TxHash,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        // TODO: we probably don't need to hash the hash.
        let expected = hash_to_field(tx_hash.as_slice());
        if semaphore_proof.signal_hash != expected {
            return Err(WorldChainTransactionPoolInvalid::InvalidSignalHash.into());
        }
        Ok(())
    }

    pub fn validate_semaphore_proof(
        &self,
        transaction: &Tx,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let date = chrono::Utc::now();
        self.validate_root(semaphore_proof)?;
        self.validate_external_nullifier(date, &semaphore_proof.external_nullifier)?;
        self.validate_nullifier(semaphore_proof)?;
        self.validate_signal_hash(transaction.hash(), semaphore_proof)?;

        // TODO: Think about DOS mitigation.
        let res = verify_proof(
            semaphore_proof.root,
            semaphore_proof.nullifier_hash,
            semaphore_proof.signal_hash,
            semaphore_proof.external_nullifier_hash,
            &semaphore_proof.proof.0,
            30,
        );

        match res {
            Ok(true) => Ok(()),
            Ok(false) => Err(WorldChainTransactionPoolInvalid::InvalidSemaphoreProof.into()),
            Err(e) => Err(TransactionValidationError::Error(e.into())),
        }
    }

    pub fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if let Some(semaphore_proof) = transaction.semaphore_proof() {
            if let Err(e) = self.validate_semaphore_proof(&transaction, semaphore_proof) {
                return e.to_outcome(transaction);
            }
            match self.set_validated(&transaction, semaphore_proof) {
                Ok(_) => {}
                Err(DatabaseError::Write(write)) => {
                    if let DatabaseWriteOperation::CursorInsert = write.operation {
                        return Into::<TransactionValidationError>::into(
                            WorldChainTransactionPoolInvalid::DuplicateTxHash,
                        )
                        .to_outcome(transaction);
                    } else {
                        return Into::<TransactionValidationError>::into(
                            WorldChainTransactionPoolError::Database(DatabaseError::Write(write)),
                        )
                        .to_outcome(transaction);
                    }
                }
                Err(e) => {
                    return Into::<TransactionValidationError>::into(
                        WorldChainTransactionPoolError::Database(e),
                    )
                    .to_outcome(transaction);
                }
            }
        }
        self.inner.validate_one(origin, transaction)
    }

    /// Validates all given transactions.
    ///
    /// Returns all outcomes for the given transactions in the same order.
    ///
    /// See also [`Self::validate_one`]
    pub fn validate_all(
        &self,
        transactions: Vec<(TransactionOrigin, Tx)>,
    ) -> Vec<TransactionValidationOutcome<Tx>> {
        transactions
            .into_iter()
            .map(|(origin, tx)| self.validate_one(origin, tx))
            .collect()
    }
}

impl<Client, Tx> TransactionValidator for WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WorldChainPoolTransaction,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        self.validate_one(origin, transaction)
    }

    async fn validate_transactions(
        &self,
        transactions: Vec<(TransactionOrigin, Self::Transaction)>,
    ) -> Vec<TransactionValidationOutcome<Self::Transaction>> {
        self.validate_all(transactions)
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock) {
        self.inner.on_new_head_block(new_tip_block)
    }
}

fn format_date(date: DateTime<chrono::Utc>) -> String {
    format!("{:0>2}{}", date.month(), date.year())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::TxKind;
    use chrono::{TimeZone, Utc};
    use ethers_core::types::U256;
    use reth_chainspec::MAINNET;
    use reth_node_optimism::txpool::OpTransactionValidator;
    use reth_primitives::{
        PooledTransactionsElement, Signature, Transaction, TransactionSigned,
        TransactionSignedEcRecovered, TxDeposit,
    };
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
        EthPooledTransaction, TransactionOrigin, TransactionValidationOutcome,
    };
    use reth_transaction_pool::{Pool, PoolTransaction as _, TransactionPool};
    use revm_primitives::hex;
    use semaphore::identity::Identity;
    use semaphore::poseidon_tree::LazyPoseidonTree;
    use semaphore::protocol::{generate_nullifier_hash, generate_proof};
    use semaphore::{hash_to_field, Field};
    use tempfile::tempdir;

    use crate::pbh::db::load_world_chain_db;
    use crate::pbh::semaphore::{Proof, SemaphoreProof};
    use crate::pbh::tx::Prefix;
    use crate::pool::ordering::WorldChainOrdering;
    use crate::pool::tx::WorldChainPooledTransaction;
    use crate::pool::validator::WorldChainTransactionValidator;

    use super::format_date;

    fn get_eth_transaction() -> EthPooledTransaction {
        let raw = "0x02f914950181ad84b2d05e0085117553845b830f7df88080b9143a6040608081523462000414576200133a803803806200001e8162000419565b9283398101608082820312620004145781516001600160401b03908181116200041457826200004f9185016200043f565b92602092838201519083821162000414576200006d9183016200043f565b8186015190946001600160a01b03821692909183900362000414576060015190805193808511620003145760038054956001938488811c9816801562000409575b89891014620003f3578190601f988981116200039d575b50899089831160011462000336576000926200032a575b505060001982841b1c191690841b1781555b8751918211620003145760049788548481811c9116801562000309575b89821014620002f457878111620002a9575b5087908784116001146200023e5793839491849260009562000232575b50501b92600019911b1c19161785555b6005556007805460ff60a01b19169055600880546001600160a01b0319169190911790553015620001f3575060025469d3c21bcecceda100000092838201809211620001de57506000917fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef9160025530835282815284832084815401905584519384523093a351610e889081620004b28239f35b601190634e487b7160e01b6000525260246000fd5b90606493519262461bcd60e51b845283015260248201527f45524332303a206d696e7420746f20746865207a65726f2061646472657373006044820152fd5b0151935038806200013a565b9190601f198416928a600052848a6000209460005b8c8983831062000291575050501062000276575b50505050811b0185556200014a565b01519060f884600019921b161c191690553880808062000267565b86860151895590970196948501948893500162000253565b89600052886000208880860160051c8201928b8710620002ea575b0160051c019085905b828110620002dd5750506200011d565b60008155018590620002cd565b92508192620002c4565b60228a634e487b7160e01b6000525260246000fd5b90607f16906200010b565b634e487b7160e01b600052604160045260246000fd5b015190503880620000dc565b90869350601f19831691856000528b6000209260005b8d8282106200038657505084116200036d575b505050811b018155620000ee565b015160001983861b60f8161c191690553880806200035f565b8385015186558a979095019493840193016200034c565b90915083600052896000208980850160051c8201928c8610620003e9575b918891869594930160051c01915b828110620003d9575050620000c5565b60008155859450889101620003c9565b92508192620003bb565b634e487b7160e01b600052602260045260246000fd5b97607f1697620000ae565b600080fd5b6040519190601f01601f191682016001600160401b038111838210176200031457604052565b919080601f84011215620004145782516001600160401b038111620003145760209062000475601f8201601f1916830162000419565b92818452828287010111620004145760005b8181106200049d57508260009394955001015290565b85810183015184820184015282016200048756fe608060408181526004918236101561001657600080fd5b600092833560e01c91826306fdde0314610a1c57508163095ea7b3146109f257816318160ddd146109d35781631b4c84d2146109ac57816323b872dd14610833578163313ce5671461081757816339509351146107c357816370a082311461078c578163715018a6146107685781638124f7ac146107495781638da5cb5b1461072057816395d89b411461061d578163a457c2d714610575578163a9059cbb146104e4578163c9567bf914610120575063dd62ed3e146100d557600080fd5b3461011c578060031936011261011c57806020926100f1610b5a565b6100f9610b75565b6001600160a01b0391821683526001865283832091168252845220549051908152f35b5080fd5b905082600319360112610338576008546001600160a01b039190821633036104975760079283549160ff8360a01c1661045557737a250d5630b4cf539739df2c5dacb4c659f2488d92836bffffffffffffffffffffffff60a01b8092161786553087526020938785528388205430156104065730895260018652848920828a52865280858a205584519081527f8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925863092a38554835163c45a015560e01b815290861685828581845afa9182156103dd57849187918b946103e7575b5086516315ab88c960e31b815292839182905afa9081156103dd576044879289928c916103c0575b508b83895196879586946364e329cb60e11b8652308c870152166024850152165af19081156103b6579086918991610389575b50169060065416176006558385541660604730895288865260c4858a20548860085416928751958694859363f305d71960e01b8552308a86015260248501528d60448501528d606485015260848401524260a48401525af1801561037f579084929161034c575b50604485600654169587541691888551978894859363095ea7b360e01b855284015260001960248401525af1908115610343575061030c575b5050805460ff60a01b1916600160a01b17905580f35b81813d831161033c575b6103208183610b8b565b8101031261033857518015150361011c5738806102f6565b8280fd5b503d610316565b513d86823e3d90fd5b6060809293503d8111610378575b6103648183610b8b565b81010312610374578290386102bd565b8580fd5b503d61035a565b83513d89823e3d90fd5b6103a99150863d88116103af575b6103a18183610b8b565b810190610e33565b38610256565b503d610397565b84513d8a823e3d90fd5b6103d79150843d86116103af576103a18183610b8b565b38610223565b85513d8b823e3d90fd5b6103ff919450823d84116103af576103a18183610b8b565b92386101fb565b845162461bcd60e51b81528085018790526024808201527f45524332303a20617070726f76652066726f6d20746865207a65726f206164646044820152637265737360e01b6064820152608490fd5b6020606492519162461bcd60e51b8352820152601760248201527f74726164696e6720697320616c7265616479206f70656e0000000000000000006044820152fd5b608490602084519162461bcd60e51b8352820152602160248201527f4f6e6c79206f776e65722063616e2063616c6c20746869732066756e6374696f6044820152603760f91b6064820152fd5b9050346103385781600319360112610338576104fe610b5a565b9060243593303303610520575b602084610519878633610bc3565b5160018152f35b600594919454808302908382041483151715610562576127109004820391821161054f5750925080602061050b565b634e487b7160e01b815260118552602490fd5b634e487b7160e01b825260118652602482fd5b9050823461061a578260031936011261061a57610590610b5a565b918360243592338152600160205281812060018060a01b03861682526020522054908282106105c9576020856105198585038733610d31565b608490602086519162461bcd60e51b8352820152602560248201527f45524332303a2064656372656173656420616c6c6f77616e63652062656c6f77604482015264207a65726f60d81b6064820152fd5b80fd5b83833461011c578160031936011261011c57805191809380549160019083821c92828516948515610716575b6020958686108114610703578589529081156106df5750600114610687575b6106838787610679828c0383610b8b565b5191829182610b11565b0390f35b81529295507f8a35acfbc15ff81a39ae7d344fd709f28e8600b4aa8c65c6b64bfe7fe36bd19b5b8284106106cc57505050826106839461067992820101948680610668565b80548685018801529286019281016106ae565b60ff19168887015250505050151560051b8301019250610679826106838680610668565b634e487b7160e01b845260228352602484fd5b93607f1693610649565b50503461011c578160031936011261011c5760085490516001600160a01b039091168152602090f35b50503461011c578160031936011261011c576020906005549051908152f35b833461061a578060031936011261061a57600880546001600160a01b031916905580f35b50503461011c57602036600319011261011c5760209181906001600160a01b036107b4610b5a565b16815280845220549051908152f35b82843461061a578160031936011261061a576107dd610b5a565b338252600160209081528383206001600160a01b038316845290528282205460243581019290831061054f57602084610519858533610d31565b50503461011c578160031936011261011c576020905160128152f35b83833461011c57606036600319011261011c5761084e610b5a565b610856610b75565b6044359160018060a01b0381169485815260209560018752858220338352875285822054976000198903610893575b505050906105199291610bc3565b85891061096957811561091a5733156108cc5750948481979861051997845260018a528284203385528a52039120558594938780610885565b865162461bcd60e51b8152908101889052602260248201527f45524332303a20617070726f766520746f20746865207a65726f206164647265604482015261737360f01b6064820152608490fd5b865162461bcd60e51b81529081018890526024808201527f45524332303a20617070726f76652066726f6d20746865207a65726f206164646044820152637265737360e01b6064820152608490fd5b865162461bcd60e51b8152908101889052601d60248201527f45524332303a20696e73756666696369656e7420616c6c6f77616e63650000006044820152606490fd5b50503461011c578160031936011261011c5760209060ff60075460a01c1690519015158152f35b50503461011c578160031936011261011c576020906002549051908152f35b50503461011c578060031936011261011c57602090610519610a12610b5a565b6024359033610d31565b92915034610b0d5783600319360112610b0d57600354600181811c9186908281168015610b03575b6020958686108214610af05750848852908115610ace5750600114610a75575b6106838686610679828b0383610b8b565b929550600383527fc2575a0e9e593c00f959f8c92f12db2869c3395a3b0502d05e2516446f71f85b5b828410610abb575050508261068394610679928201019438610a64565b8054868501880152928601928101610a9e565b60ff191687860152505050151560051b83010192506106798261068338610a64565b634e487b7160e01b845260229052602483fd5b93607f1693610a44565b8380fd5b6020808252825181830181905290939260005b828110610b4657505060409293506000838284010152601f8019910116010190565b818101860151848201604001528501610b24565b600435906001600160a01b0382168203610b7057565b600080fd5b602435906001600160a01b0382168203610b7057565b90601f8019910116810190811067ffffffffffffffff821117610bad57604052565b634e487b7160e01b600052604160045260246000fd5b6001600160a01b03908116918215610cde5716918215610c8d57600082815280602052604081205491808310610c3957604082827fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef958760209652828652038282205586815220818154019055604051908152a3565b60405162461bcd60e51b815260206004820152602660248201527f45524332303a207472616e7366657220616d6f756e7420657863656564732062604482015265616c616e636560d01b6064820152608490fd5b60405162461bcd60e51b815260206004820152602360248201527f45524332303a207472616e7366657220746f20746865207a65726f206164647260448201526265737360e81b6064820152608490fd5b60405162461bcd60e51b815260206004820152602560248201527f45524332303a207472616e736665722066726f6d20746865207a65726f206164604482015264647265737360d81b6064820152608490fd5b6001600160a01b03908116918215610de25716918215610d925760207f8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925918360005260018252604060002085600052825280604060002055604051908152a3565b60405162461bcd60e51b815260206004820152602260248201527f45524332303a20617070726f766520746f20746865207a65726f206164647265604482015261737360f01b6064820152608490fd5b60405162461bcd60e51b8152602060048201526024808201527f45524332303a20617070726f76652066726f6d20746865207a65726f206164646044820152637265737360e01b6064820152608490fd5b90816020910312610b7057516001600160a01b0381168103610b70579056fea2646970667358221220285c200b3978b10818ff576bb83f2dc4a2a7c98dfb6a36ea01170de792aa652764736f6c63430008140033000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000c0000000000000000000000000d3fd4f95820a9aa848ce716d6c200eaefb9a2e4900000000000000000000000000000000000000000000000000000000000000640000000000000000000000000000000000000000000000000000000000000003543131000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000035431310000000000000000000000000000000000000000000000000000000000c001a04e551c75810ffdfe6caff57da9f5a8732449f42f0f4c57f935b05250a76db3b6a046cd47e6d01914270c1ec0d9ac7fae7dfb240ec9a8b6ec7898c4d6aa174388f2";

        let data = hex::decode(raw).unwrap();
        let tx = PooledTransactionsElement::decode_enveloped(&mut data.as_ref()).unwrap();

        tx.try_into_ecrecovered().unwrap().into()
    }

    fn get_non_pbh_transaction() -> WorldChainPooledTransaction {
        let eth_tx = get_eth_transaction();
        WorldChainPooledTransaction {
            inner: eth_tx,
            semaphore_proof: None,
        }
    }

    fn get_pbh_transaction() -> WorldChainPooledTransaction {
        let eth_tx = get_eth_transaction();
        let semaphore_proof = valid_proof(
            &mut [0; 32],
            eth_tx.hash().as_slice(),
            chrono::Utc::now(),
            0,
        );
        WorldChainPooledTransaction {
            inner: eth_tx,
            semaphore_proof: Some(semaphore_proof),
        }
    }

    fn world_chain_validator(
    ) -> WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction> {
        let client = MockEthProvider::default();
        let validator = EthTransactionValidatorBuilder::new(MAINNET.clone())
            .no_shanghai()
            .no_cancun()
            .build(client, InMemoryBlobStore::default());
        let validator = OpTransactionValidator::new(validator).require_l1_data_gas_fee(false);
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("db");
        let db = load_world_chain_db(&path, false).unwrap();
        WorldChainTransactionValidator::new(validator, db, 30)
    }

    fn valid_proof(
        identity: &mut [u8],
        tx_hash: &[u8],
        time: chrono::DateTime<Utc>,
        pbh_nonce: u16,
    ) -> SemaphoreProof {
        let date_str = format_date(time);
        let external_nullifier = format!("{}-{}-{}", Prefix::V1, date_str, pbh_nonce);
        create_proof(identity, external_nullifier, tx_hash, 30)
    }

    fn create_proof(
        identity: &mut [u8],
        external_nullifier: String,
        signal: &[u8],
        depth: usize,
    ) -> SemaphoreProof {
        let leaf = Field::from(0);

        // generate identity
        let id = Identity::from_secret(identity, None);

        // generate merkle tree
        let mut tree = LazyPoseidonTree::new(depth, leaf).derived();
        tree = tree.update(0, &id.commitment());

        let merkle_proof = tree.proof(0);

        let signal_hash = hash_to_field(signal);
        let external_nullifier_hash = hash_to_field(external_nullifier.as_bytes());
        let nullifier_hash = generate_nullifier_hash(&id, external_nullifier_hash);

        let proof = Proof(
            generate_proof(&id, &merkle_proof, external_nullifier_hash, signal_hash).unwrap(),
        );

        SemaphoreProof {
            root: tree.root(),
            nullifier_hash,
            signal_hash,
            external_nullifier,
            proof,
            external_nullifier_hash,
        }
    }

    #[tokio::test]
    async fn validate_non_pbh_transaction() {
        let validator = world_chain_validator();
        let transaction = get_non_pbh_transaction();

        validator.inner.client().add_account(
            transaction.sender(),
            ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
        );

        let outcome = validator.validate_one(TransactionOrigin::External, transaction.clone());
        assert!(outcome.is_valid());

        let ordering = WorldChainOrdering::new(validator.database_env.clone());

        let pool = Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        );

        let res = pool.add_external_transaction(transaction.clone()).await;
        assert!(res.is_ok());
        let tx = pool.get(transaction.hash());
        assert!(tx.is_some());
    }

    #[tokio::test]
    async fn validate_pbh_transaction() {
        let validator = world_chain_validator();
        let transaction = get_pbh_transaction();
        validator.inner.client().add_account(
            transaction.sender(),
            ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
        );

        let ordering = WorldChainOrdering::new(validator.database_env.clone());

        let pool = Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        );

        let res = pool.add_external_transaction(transaction.clone()).await;
        println!("{:?}", res);
        assert!(res.is_ok());
        let tx = pool.get(transaction.hash());
        assert!(tx.is_some());
    }

    #[tokio::test]
    async fn invalid_external_nullifier_hash() {
        let validator = world_chain_validator();
        let mut transaction = get_pbh_transaction();
        transaction
            .semaphore_proof
            .as_mut()
            .unwrap()
            .external_nullifier_hash = Field::from(0);

        validator.inner.client().add_account(
            transaction.sender(),
            ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
        );

        let ordering = WorldChainOrdering::new(validator.database_env.clone());

        let pool = Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        );

        let res = pool.add_external_transaction(transaction.clone()).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn invalid_signal_hash() {
        let validator = world_chain_validator();
        let mut transaction = get_pbh_transaction();
        transaction.semaphore_proof.as_mut().unwrap().signal_hash = Field::from(0);

        validator.inner.client().add_account(
            transaction.sender(),
            ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
        );

        let ordering = WorldChainOrdering::new(validator.database_env.clone());

        let pool = Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        );

        let res = pool.add_external_transaction(transaction.clone()).await;
        assert!(res.is_err());
    }

    #[test]
    fn test_format_date() {
        let date = chrono::Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();
        let formated = super::format_date(date);
        let expected = "012021".to_string();
        assert_eq!(formated, expected);
    }

    #[test]
    fn test_validate_external_nullifier() {
        let validator = world_chain_validator();
        let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let valid_external_nullifiers = ["v1-012025-0", "v1-012025-1", "v1-012025-29"];
        let invalid_external_nullifiers = [
            "v0-012025-0",
            "v1-022025-0",
            "v1-002025-0",
            "v1-012025-30",
            "v1-012025",
            "12025-0",
            "v1-012025-0-0",
        ];
        for valid in valid_external_nullifiers.iter() {
            validator.validate_external_nullifier(date, valid).unwrap();
        }
        for invalid in invalid_external_nullifiers.iter() {
            let res = validator.validate_external_nullifier(date, invalid);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_set_validated() {
        let validator = world_chain_validator();

        let proof = Proof(semaphore::protocol::Proof(
            (U256::from(1u64), U256::from(2u64)),
            (
                [U256::from(3u64), U256::from(4u64)],
                [U256::from(5u64), U256::from(6u64)],
            ),
            (U256::from(7u64), U256::from(8u64)),
        ));
        let semaphore_proof = SemaphoreProof {
            external_nullifier: "0-012025-11".to_string(),
            external_nullifier_hash: Field::from(9u64),
            nullifier_hash: Field::from(10u64),
            signal_hash: Field::from(11u64),
            root: Field::from(12u64),
            proof,
        };
        let tx = TransactionSignedEcRecovered::default();
        let inner = EthPooledTransaction::new(tx, 0);
        let tx = WorldChainPooledTransaction {
            inner,
            semaphore_proof: Some(semaphore_proof.clone()),
        };

        validator.set_validated(&tx, &semaphore_proof).unwrap();
    }

    #[test]
    fn validate_optimism_transaction() {
        let validator = world_chain_validator();
        let origin = TransactionOrigin::External;
        let signer = Default::default();
        let deposit_tx = Transaction::Deposit(TxDeposit {
            source_hash: Default::default(),
            from: signer,
            to: TxKind::Create,
            mint: None,
            value: revm_primitives::ruint::aliases::U256::ZERO,
            gas_limit: 0,
            is_system_transaction: false,
            input: Default::default(),
        });
        let signature = Signature::default();
        let signed_tx = TransactionSigned::from_transaction_and_signature(deposit_tx, signature);
        let signed_recovered =
            TransactionSignedEcRecovered::from_signed_transaction(signed_tx, signer);
        let len = signed_recovered.length_without_header();
        let pooled_tx = EthPooledTransaction::new(signed_recovered, len);
        let world_chain_pooled_tx = WorldChainPooledTransaction {
            inner: pooled_tx,
            semaphore_proof: None,
        };
        let outcome = validator.validate_one(origin, world_chain_pooled_tx);

        let err = match outcome {
            TransactionValidationOutcome::Invalid(_, err) => err,
            _ => panic!("Expected invalid transaction"),
        };
        assert_eq!(err.to_string(), "transaction type not supported");
    }
}
