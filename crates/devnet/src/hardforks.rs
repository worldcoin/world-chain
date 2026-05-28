use std::collections::BTreeSet;

use eyre::eyre::{Result, bail};
use reth_chainspec::{EthereumHardfork, ForkCondition};
use world_chain_chainspec::{WorldChainHardfork, WorldChainSpec};

/// Canonical hardfork order for local World Chain devnets.
pub const WORLD_CHAIN_DEVNET_HARDFORK_ORDER: [WorldChainHardfork; 11] = [
    WorldChainHardfork::Bedrock,
    WorldChainHardfork::Regolith,
    WorldChainHardfork::Canyon,
    WorldChainHardfork::Ecotone,
    WorldChainHardfork::Fjord,
    WorldChainHardfork::Granite,
    WorldChainHardfork::Holocene,
    WorldChainHardfork::Isthmus,
    WorldChainHardfork::Jovian,
    WorldChainHardfork::Tropo,
    WorldChainHardfork::Strato,
];

/// Typed World Chain hardfork selection for local devnets.
///
/// Defaults match current local-dev expectations: all OP/World hardforks
/// through Jovian are active at genesis, while the World-specific Tropo and
/// Strato forks are disabled until explicitly selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldChainHardforkConfig {
    active: BTreeSet<WorldChainHardfork>,
}

impl Default for WorldChainHardforkConfig {
    fn default() -> Self {
        Self::through(WorldChainHardfork::Jovian)
    }
}

impl WorldChainHardforkConfig {
    /// Enable all forks up to and including `latest`.
    pub fn through(latest: WorldChainHardfork) -> Self {
        let active = WORLD_CHAIN_DEVNET_HARDFORK_ORDER
            .into_iter()
            .take_while(|fork| fork.idx() <= latest.idx())
            .collect();
        Self { active }
    }

    /// Return true if `fork` is active at genesis.
    pub fn is_active(&self, fork: WorldChainHardfork) -> bool {
        self.active.contains(&fork)
    }

    /// Enable an individual hardfork.
    pub fn enable(mut self, fork: WorldChainHardfork) -> Self {
        self.active.insert(fork);
        self
    }

    /// Disable an individual hardfork.
    pub fn disable(mut self, fork: WorldChainHardfork) -> Self {
        self.active.remove(&fork);
        self
    }

    /// Active hardforks in canonical order.
    pub fn active(&self) -> impl Iterator<Item = WorldChainHardfork> + '_ {
        WORLD_CHAIN_DEVNET_HARDFORK_ORDER
            .into_iter()
            .filter(|fork| self.active.contains(fork))
    }

    /// Validate that the selected hardforks form a prefix of the canonical order.
    pub fn validate(&self) -> Result<()> {
        let mut seen_inactive = false;
        for fork in WORLD_CHAIN_DEVNET_HARDFORK_ORDER {
            if self.active.contains(&fork) {
                if seen_inactive {
                    bail!(
                        "invalid World Chain hardfork selection: {} is active after an earlier fork was disabled",
                        fork.name()
                    );
                }
            } else {
                seen_inactive = true;
            }
        }
        Ok(())
    }

    /// Apply this selection to a chain spec.
    pub fn apply_to(&self, mut spec: WorldChainSpec) -> WorldChainSpec {
        for fork in WORLD_CHAIN_DEVNET_HARDFORK_ORDER {
            let condition = if self.active.contains(&fork) {
                match fork {
                    WorldChainHardfork::Bedrock => ForkCondition::Block(0),
                    _ => ForkCondition::Timestamp(0),
                }
            } else {
                ForkCondition::Never
            };
            spec.set_fork(fork, condition);
        }

        spec.set_fork(
            EthereumHardfork::Shanghai,
            if self.active.contains(&WorldChainHardfork::Canyon) {
                ForkCondition::Timestamp(0)
            } else {
                ForkCondition::Never
            },
        );
        spec.set_fork(
            EthereumHardfork::Cancun,
            if self.active.contains(&WorldChainHardfork::Ecotone) {
                ForkCondition::Timestamp(0)
            } else {
                ForkCondition::Never
            },
        );
        spec.set_fork(
            EthereumHardfork::Prague,
            if self.active.contains(&WorldChainHardfork::Isthmus) {
                ForkCondition::Timestamp(0)
            } else {
                ForkCondition::Never
            },
        );

        spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_local_devnet_hardforks_stop_at_jovian() {
        let hardforks = WorldChainHardforkConfig::default();

        assert!(hardforks.is_active(WorldChainHardfork::Jovian));
        assert!(!hardforks.is_active(WorldChainHardfork::Tropo));
        assert!(!hardforks.is_active(WorldChainHardfork::Strato));
    }

    #[test]
    fn validates_prefix_only_hardfork_selection() {
        let valid = WorldChainHardforkConfig::through(WorldChainHardfork::Ecotone);
        assert!(valid.validate().is_ok());

        let invalid = valid.enable(WorldChainHardfork::Jovian);
        assert!(invalid.validate().is_err());
    }
}
