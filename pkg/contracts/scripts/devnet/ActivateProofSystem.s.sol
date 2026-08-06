// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {GameTypes} from "../../src/proofs/GameTypes.sol";
import {IMultiProofGame} from "../../src/proofs/interfaces/IMultiProofGame.sol";

import {GameStatus} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

/// @notice Activates an already-registered WIP-1006 implementation after verifying that its
///         wiring, bond and current ASR anchor allow new games to be created safely.
/// @dev This script never changes the ASR retirement timestamp. A nonzero anchor must satisfy
///      the same parent-validity conditions enforced by `MultiProofGame.initialize()`.
contract ActivateProofSystem is Script {
    struct Config {
        uint256 guardianKey;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        ISystemConfig systemConfig;
        bool requireFreshAnchor;
    }

    function run() external returns (IMultiProofGame gameImpl) {
        Config memory config = _readConfig();
        gameImpl = IMultiProofGame(address(config.disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE)));
        _validate(config, gameImpl);

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
        config.guardianKey = vm.envUint("GUARDIAN_KEY");
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.anchorStateRegistry = IAnchorStateRegistry(vm.envAddress("ANCHOR_STATE_REGISTRY"));
        config.systemConfig = ISystemConfig(vm.envAddress("SYSTEM_CONFIG"));
        config.requireFreshAnchor = vm.envOr("REQUIRE_FRESH_ANCHOR", false);
    }

    function _validate(Config memory config, IMultiProofGame gameImpl) internal view {
        require(config.guardianKey != 0, "ActivateProofSystem: guardian key required");
        require(
            vm.addr(config.guardianKey) == config.systemConfig.guardian(), "ActivateProofSystem: guardian key mismatch"
        );
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
            config.disputeGameFactory.initBonds(GameTypes.MULTI_PROOF_GAME_TYPE) == gameImpl.proposerBond(),
            "ActivateProofSystem: init bond mismatch"
        );
        require(
            address(gameImpl.validityProofVerifier()).code.length > 0, "ActivateProofSystem: validity verifier missing"
        );
        require(address(gameImpl.teeVerifier()).code.length > 0, "ActivateProofSystem: TEE verifier missing");
        require(address(gameImpl.securityCouncil()).code.length > 0, "ActivateProofSystem: council verifier missing");
        require(address(gameImpl.weth()).code.length > 0, "ActivateProofSystem: DelayedWETH missing");

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
}
