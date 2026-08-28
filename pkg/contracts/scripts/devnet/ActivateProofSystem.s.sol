// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";

import {GameStatus} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @notice Registers and activates a WIP-1006 implementation after verifying that its
///         wiring, vault and current ASR anchor allow new games to be created safely.
/// @dev This script never changes the ASR retirement timestamp. A nonzero anchor must satisfy
///      the same parent-validity conditions enforced by `MultiProofGame.initialize()`.
contract ActivateProofSystem is Script {
    struct Config {
        uint256 guardianKey;
        uint256 dgfOwnerKey;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        ISystemConfig systemConfig;
        IProxyAdmin proxyAdmin;
        IERC20 wld;
        IMultiProofGame gameImplementation;
        bool requireFreshAnchor;
    }

    function run() external returns (IMultiProofGame gameImpl) {
        Config memory config = _readConfig();
        gameImpl = config.gameImplementation;
        _validate(config, gameImpl);

        vm.startBroadcast(config.dgfOwnerKey);
        config.disputeGameFactory
            .setImplementation(GameTypes.MULTI_PROOF_GAME_TYPE, IDisputeGame(address(gameImpl)), hex"");
        // Register first so a migration from an ETH-bonded implementation fails closed during
        // the transaction gap instead of briefly allowing zero-bond legacy games.
        config.disputeGameFactory.setInitBond(GameTypes.MULTI_PROOF_GAME_TYPE, 0);
        vm.stopBroadcast();

        require(
            address(config.disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE)) == address(gameImpl),
            "ActivateProofSystem: game implementation not registered"
        );
        require(
            config.disputeGameFactory.gameArgs(GameTypes.MULTI_PROOF_GAME_TYPE).length == 0,
            "ActivateProofSystem: stale game implementation args"
        );
        require(
            config.disputeGameFactory.initBonds(GameTypes.MULTI_PROOF_GAME_TYPE) == 0,
            "ActivateProofSystem: factory bond must be zero"
        );

        if (config.anchorStateRegistry.respectedGameType().raw() != GameTypes.MULTI_PROOF_GAME_TYPE.raw()) {
            vm.startBroadcast(config.guardianKey);
            config.anchorStateRegistry.setRespectedGameType(GameTypes.MULTI_PROOF_GAME_TYPE);
            vm.stopBroadcast();
        }

        require(
            config.anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw(),
            "ActivateProofSystem: respected game type not activated"
        );
    }

    function _readConfig() internal view returns (Config memory config) {
        config.guardianKey = vm.envOr("GUARDIAN_KEY", uint256(0));
        config.dgfOwnerKey = vm.envUint("DGF_OWNER_KEY");
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.anchorStateRegistry = IAnchorStateRegistry(vm.envAddress("ANCHOR_STATE_REGISTRY"));
        config.systemConfig = ISystemConfig(vm.envAddress("SYSTEM_CONFIG"));
        config.proxyAdmin = IProxyAdmin(vm.envAddress("OP_CHAIN_PROXY_ADMIN"));
        config.wld = IERC20(vm.envAddress("WLD_TOKEN"));
        config.gameImplementation = IMultiProofGame(vm.envAddress("GAME_IMPLEMENTATION"));
        config.requireFreshAnchor = vm.envOr("REQUIRE_FRESH_ANCHOR", false);
    }

    function _validate(Config memory config, IMultiProofGame gameImpl) internal view {
        require(config.dgfOwnerKey != 0, "ActivateProofSystem: DGF owner key required");
        require(
            vm.addr(config.dgfOwnerKey) == config.disputeGameFactory.owner(),
            "ActivateProofSystem: DGF owner key mismatch"
        );
        require(
            config.disputeGameFactory.owner() == config.proxyAdmin.owner(),
            "ActivateProofSystem: DGF and ProxyAdmin owners must match"
        );
        if (config.anchorStateRegistry.respectedGameType().raw() != GameTypes.MULTI_PROOF_GAME_TYPE.raw()) {
            require(config.guardianKey != 0, "ActivateProofSystem: guardian key required");
            require(
                vm.addr(config.guardianKey) == config.systemConfig.guardian(),
                "ActivateProofSystem: guardian key mismatch"
            );
        }
        require(address(gameImpl).code.length > 0, "ActivateProofSystem: game implementation required");
        require(
            gameImpl.gameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw(), "ActivateProofSystem: wrong game type"
        );
        require(
            address(config.anchorStateRegistry.disputeGameFactory()) == address(config.disputeGameFactory),
            "ActivateProofSystem: ASR factory mismatch"
        );
        require(
            address(config.anchorStateRegistry.systemConfig()) == address(config.systemConfig),
            "ActivateProofSystem: ASR SystemConfig mismatch"
        );
        require(
            address(gameImpl.disputeGameFactory()) == address(config.disputeGameFactory),
            "ActivateProofSystem: game factory mismatch"
        );
        require(
            address(gameImpl.anchorStateRegistry()) == address(config.anchorStateRegistry),
            "ActivateProofSystem: game ASR mismatch"
        );
        require(
            address(gameImpl.validityProofVerifier()).code.length > 0, "ActivateProofSystem: validity verifier missing"
        );
        require(address(gameImpl.teeVerifier()).code.length > 0, "ActivateProofSystem: TEE verifier missing");
        require(address(gameImpl.securityCouncil()).code.length > 0, "ActivateProofSystem: council verifier missing");
        require(
            gameImpl.validityProofVerifier() != gameImpl.teeVerifier()
                && gameImpl.validityProofVerifier() != gameImpl.securityCouncil()
                && gameImpl.teeVerifier() != gameImpl.securityCouncil(),
            "ActivateProofSystem: proof lane verifiers must be distinct"
        );
        require(address(gameImpl.bondVault()).code.length > 0, "ActivateProofSystem: WLD staking vault missing");
        require(gameImpl.proposerBond() > 0, "ActivateProofSystem: proposer bond missing");
        require(gameImpl.challengerBond() > 0, "ActivateProofSystem: challenger bond missing");
        require(
            address(gameImpl.bondVault().disputeGameFactory()) == address(config.disputeGameFactory),
            "ActivateProofSystem: vault factory mismatch"
        );
        require(
            address(gameImpl.bondVault().systemConfig()) == address(config.systemConfig),
            "ActivateProofSystem: vault SystemConfig mismatch"
        );
        require(gameImpl.bondVault().wld() == config.wld, "ActivateProofSystem: vault WLD mismatch");
        require(address(config.wld).code.length > 0, "ActivateProofSystem: WLD token missing");
        require(
            gameImpl.bondVault().proxyAdmin() == config.proxyAdmin, "ActivateProofSystem: vault ProxyAdmin mismatch"
        );

        IWLDStakingVault currentBondVault = _currentBondVault(config.disputeGameFactory);
        require(
            address(currentBondVault) == address(0) || currentBondVault == gameImpl.bondVault(),
            "ActivateProofSystem: must reuse current WLD vault"
        );
        require(gameImpl.aggregationVKey() != bytes32(0), "ActivateProofSystem: aggregation vkey missing");
        require(gameImpl.rangeVKeyCommitment() != bytes32(0), "ActivateProofSystem: range vkey missing");
        require(gameImpl.teeImageId() != bytes32(0), "ActivateProofSystem: TEE image ID missing");

        IDisputeGame anchorGame = config.anchorStateRegistry.anchorGame();
        if (config.requireFreshAnchor) {
            require(address(anchorGame) == address(0), "ActivateProofSystem: fresh bootstrap has anchor game");
        } else if (address(anchorGame) != address(0)) {
            require(
                config.anchorStateRegistry.isGameRegistered(anchorGame),
                "ActivateProofSystem: anchor game not registered"
            );
            require(
                config.anchorStateRegistry.isGameRespected(anchorGame),
                "ActivateProofSystem: anchor game was not respected"
            );
            require(
                !config.anchorStateRegistry.isGameBlacklisted(anchorGame),
                "ActivateProofSystem: anchor game blacklisted"
            );
            require(!config.anchorStateRegistry.isGameRetired(anchorGame), "ActivateProofSystem: anchor game retired");
            require(anchorGame.status() != GameStatus.CHALLENGER_WINS, "ActivateProofSystem: anchor game invalid");
        }
    }

    function _currentBondVault(IDisputeGameFactory factory) internal view returns (IWLDStakingVault bondVault) {
        IDisputeGame currentImplementation = factory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE);
        if (address(currentImplementation) == address(0)) return IWLDStakingVault(address(0));

        try IMultiProofGame(address(currentImplementation)).bondVault() returns (IWLDStakingVault currentBondVault) {
            bondVault = currentBondVault;
        } catch {
            // The first WLD migration may replace a legacy implementation without this getter.
        }
    }
}
