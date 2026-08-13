use reth_chainspec::{EthereumHardforks, ForkCondition, hardfork};

hardfork!(
    /// The name of a World Chain hardfork.
    ///
    /// World Chain follows the OP Stack upgrade sequence through Karst, then uses
    /// World Chain specific upgrade names as the canonical schedule diverges.
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    #[derive(Default)]
    WorldChainHardfork {
        /// Bedrock: OP Stack Bedrock upgrade.
        Bedrock,
        /// Regolith: OP Stack Regolith upgrade.
        Regolith,
        /// Canyon: OP Stack Canyon upgrade.
        Canyon,
        /// Ecotone: OP Stack Ecotone upgrade.
        Ecotone,
        /// Fjord: OP Stack Fjord upgrade.
        Fjord,
        /// Granite: OP Stack Granite upgrade.
        Granite,
        /// Holocene: OP Stack Holocene upgrade.
        Holocene,
        /// Isthmus: OP Stack Isthmus upgrade.
        Isthmus,
        /// Jovian: OP Stack Jovian upgrade. World Chain is already on this hardfork.
        #[default]
        Jovian,
        /// Karst: OP Stack Karst upgrade.
        Karst,
        /// Tropo: the first World Chain specific hardfork after Karst.
        Tropo,
        /// Strato: the second World Chain specific hardfork after Karst.
        Strato,
    }
);

impl WorldChainHardfork {
    /// Returns index of `self` in the canonical World Chain hardfork order.
    pub const fn idx(&self) -> usize {
        *self as usize
    }
}

/// Extends [`EthereumHardforks`] with World Chain hardfork helper methods.
#[auto_impl::auto_impl(&, Arc)]
pub trait WorldChainHardforks: EthereumHardforks {
    /// Retrieves [`ForkCondition`] by a [`WorldChainHardfork`]. If `fork` is not present,
    /// returns [`ForkCondition::Never`].
    fn world_chain_fork_activation(&self, fork: WorldChainHardfork) -> ForkCondition;

    /// Returns `true` if [`Bedrock`](WorldChainHardfork::Bedrock) is active at the block number.
    fn is_bedrock_active_at_block(&self, block_number: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Bedrock)
            .active_at_block(block_number)
    }

    /// Returns `true` if [`Regolith`](WorldChainHardfork::Regolith) is active.
    fn is_regolith_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Regolith)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Canyon`](WorldChainHardfork::Canyon) is active.
    fn is_canyon_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Canyon)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Ecotone`](WorldChainHardfork::Ecotone) is active.
    fn is_ecotone_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Ecotone)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Fjord`](WorldChainHardfork::Fjord) is active.
    fn is_fjord_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Fjord)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Granite`](WorldChainHardfork::Granite) is active.
    fn is_granite_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Granite)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Holocene`](WorldChainHardfork::Holocene) is active.
    fn is_holocene_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Holocene)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Isthmus`](WorldChainHardfork::Isthmus) is active.
    fn is_isthmus_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Isthmus)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Jovian`](WorldChainHardfork::Jovian) is active.
    fn is_jovian_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Jovian)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Karst`](WorldChainHardfork::Karst) is active.
    fn is_karst_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Karst)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Tropo`](WorldChainHardfork::Tropo) is active.
    fn is_tropo_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Tropo)
            .active_at_timestamp(timestamp)
    }

    /// Returns `true` if [`Strato`](WorldChainHardfork::Strato) is active.
    fn is_strato_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.world_chain_fork_activation(WorldChainHardfork::Strato)
            .active_at_timestamp(timestamp)
    }
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use super::*;

    #[test]
    fn parses_case_insensitive_hardfork_names() {
        assert_eq!(
            WorldChainHardfork::from_str("kArSt").unwrap(),
            WorldChainHardfork::Karst
        );
        assert_eq!(
            WorldChainHardfork::from_str("tRoPo").unwrap(),
            WorldChainHardfork::Tropo
        );
        assert_eq!(
            WorldChainHardfork::from_str("sTrAtO").unwrap(),
            WorldChainHardfork::Strato
        );
    }
}
