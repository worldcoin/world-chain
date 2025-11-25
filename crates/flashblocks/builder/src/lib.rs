pub mod access_list;
pub mod assembler;
pub mod block_builder;
pub mod coordinator;
pub mod executor;
pub mod payload_builder;
pub mod payload_txns;
pub mod traits;

#[cfg(feature = "test")]
pub mod test {
    use std::{collections::HashMap, sync::LazyLock};

    use alloy_primitives::Address;
    use flashblocks_primitives::access_list::FlashblockAccessList;
    use parking_lot::RwLock;
    use reth::revm::db::BundleAccount;
    use serde::{Deserialize, Serialize};
    use tracing::info;

    const PAYLOAD_TEST_CONTEXTS_DUMP_PATH: &str = "tests";

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct PayloadTestContexts(HashMap<u64, HashMap<u64, BlockContexts>>);

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct BlockContexts {
        pub executed: Option<BlockContext>,
        pub computed: Option<BlockContext>,
    }

    impl BlockContexts {
        pub fn is_complete(&self) -> bool {
            self.executed.is_some() && self.computed.is_some()
        }

        pub fn validate(&self) -> bool {
            self.executed == self.computed
        }
    }

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
    pub struct BlockContext {
        pub bundle: alloy_primitives::map::HashMap<Address, BundleAccount>,
        pub access_list: FlashblockAccessList,
    }

    pub static PAYLOAD_TEST_CONTEXTS: LazyLock<RwLock<PayloadTestContexts>> =
        LazyLock::new(|| RwLock::new(PayloadTestContexts(HashMap::new())));

    pub fn record_executed(number: u64, index: u64, executed: Option<BlockContext>) {
        insert(number, index, executed);
    }

    pub fn record_computed(number: u64, index: u64, computed: Option<BlockContext>) {
        insert(number, index, computed);
    }

    pub fn insert(number: u64, index: u64, context: Option<BlockContext>) {
        let mut contexts = PAYLOAD_TEST_CONTEXTS.write();
        let block_contexts = contexts.0.entry(number).or_default();
        let entry = block_contexts
            .entry(index)
            .and_modify(|b| {
                b.computed = context.clone();
            })
            .or_insert(BlockContexts {
                executed: context,
                computed: None,
            });

        let test_results_path = format!(
            "{}/{}",
            env!("CARGO_MANIFEST_DIR"),
            PAYLOAD_TEST_CONTEXTS_DUMP_PATH
        );

        if entry.is_complete() && !entry.validate() {
            let file_path = format!(
                "{}/failures/block-{}-{}.json",
                test_results_path, number, index
            );
            let json = serde_json::to_string_pretty(&entry).unwrap();
            std::fs::create_dir_all(&test_results_path).unwrap();
            std::fs::write(&file_path, json).unwrap();
        } else {
            let file_path = format!("block-{}-{}.json", number, index);
            let json = serde_json::to_string_pretty(&entry).unwrap();
            std::fs::write(&file_path, json).unwrap();
        }
    }
}
