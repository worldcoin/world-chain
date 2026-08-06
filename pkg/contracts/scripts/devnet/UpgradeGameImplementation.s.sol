// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";

import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";
import {LibProof} from "../../src/dispute/lib/LibProof.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";

/// @notice Swaps the `MultiProofGame` implementation registered for game type 1006 on an
///         existing `DisputeGameFactory`, without redeploying the surrounding proof system.
///
/// Every constructor parameter defaults to the value read off the *currently registered*
/// implementation, so the default run changes bytecode and nothing else. That is the point:
/// the parameters are load-bearing offchain (`domainHash` is committed into every game's
/// `extraData`, and every prover derives `rootId` from it), and re-specifying them by hand is
/// how a "bytecode-only" upgrade silently becomes a chain split between the games already in
/// flight and the workers proving them.
///
/// Each parameter can still be overridden by env var; each override is asserted and logged.
/// `domainHash` is the one value that may never move — an upgrade that changes it invalidates
/// every unresolved proposal and every proof in the prover-service queue — so it is checked
/// against the outgoing implementation and the run reverts on drift.
///
/// `ROLLUP_CONFIG_HASH` and `PROOF_SYSTEM_BLOCK_INTERVAL` are the two required inputs: the
/// implementation registered on alphanet predates the getters for them, so they cannot be
/// recovered from chain. They are not taken on trust — both feed `domainHash`, so a wrong
/// value fails the equality check above rather than shipping a forked proof domain.
///
/// Existing games are unaffected: `DisputeGameFactory.create` clones the implementation
/// registered at creation time, and a clone keeps pointing at that address. Only games created
/// after this call use the new code. Services that submit to both old and new games must
/// therefore understand both ABIs for the length of the drain window.
///
/// Rollback is the same call with `NEW_GAME_IMPLEMENTATION` set to the previous address, and
/// `setImplementation(1006, address(0))` remains the kill switch for new game creation.
contract UpgradeGameImplementation is Script {
    using SafeCast for uint256;

    struct Config {
        uint256 privateKey;
        uint256 dgfOwnerKey;
        IDisputeGameFactory disputeGameFactory;
        MultiProofGame currentImpl;
        /// @dev When set, register this pre-deployed implementation instead of deploying one.
        address preDeployedImpl;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint64 challengePeriod;
        uint64 proofPeriod;
        uint256 proposerBond;
        uint256 challengerBond;
        uint8 proofThreshold;
        address protocolFeeRecipient;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IAnchorStateRegistry anchorStateRegistry;
        IDelayedWETH weth;
    }

    function run() external returns (address newImpl) {
        Config memory config = _readConfig();

        // 1. Deploy (or adopt) the replacement implementation.
        if (config.preDeployedImpl != address(0)) {
            require(config.preDeployedImpl.code.length > 0, "Upgrade: NEW_GAME_IMPLEMENTATION has no code");
            newImpl = config.preDeployedImpl;
        } else {
            vm.startBroadcast(config.privateKey);
            newImpl = address(new MultiProofGame(_gameConfig(config)));
            vm.stopBroadcast();
        }

        _validateReplacement(config, MultiProofGame(newImpl));

        // 2. Register it on the factory. The three-argument overload clears any stale args.
        vm.startBroadcast(config.dgfOwnerKey);
        config.disputeGameFactory.setImplementation(GameTypes.MULTI_PROOF_GAME_TYPE, IDisputeGame(newImpl), hex"");
        if (config.disputeGameFactory.initBonds(GameTypes.MULTI_PROOF_GAME_TYPE) != config.proposerBond) {
            config.disputeGameFactory.setInitBond(GameTypes.MULTI_PROOF_GAME_TYPE, config.proposerBond);
        }
        vm.stopBroadcast();

        require(
            address(config.disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE)) == newImpl,
            "Upgrade: implementation not registered"
        );
        require(
            config.disputeGameFactory.gameArgs(GameTypes.MULTI_PROOF_GAME_TYPE).length == 0,
            "Upgrade: stale game implementation args"
        );
        require(
            config.disputeGameFactory.initBonds(GameTypes.MULTI_PROOF_GAME_TYPE)
                == MultiProofGame(newImpl).proposerBond(),
            "Upgrade: init bond does not match proposer bond"
        );

        _writeDeployment(config, newImpl);
    }

    function _readConfig() internal view returns (Config memory config) {
        config.disputeGameFactory = IDisputeGameFactory(vm.envAddress("DISPUTE_GAME_FACTORY"));
        config.dgfOwnerKey = vm.envUint("DGF_OWNER_KEY");
        config.privateKey = vm.envOr("PRIVATE_KEY", config.dgfOwnerKey);
        config.preDeployedImpl = vm.envOr("NEW_GAME_IMPLEMENTATION", address(0));

        address current = address(config.disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE));
        require(current != address(0), "Upgrade: no implementation registered for game type 1006");
        require(current.code.length > 0, "Upgrade: registered implementation has no code");
        config.currentImpl = MultiProofGame(current);

        require(vm.addr(config.dgfOwnerKey) == config.disputeGameFactory.owner(), "Upgrade: DGF owner key mismatch");

        // Defaults are the live values; every override is deliberate and asserted below.
        MultiProofGame prev = config.currentImpl;
        // Not readable off the deployed implementation; validated through `domainHash`.
        config.rollupConfigHash = vm.envBytes32("ROLLUP_CONFIG_HASH");
        config.blockInterval = vm.envUint("PROOF_SYSTEM_BLOCK_INTERVAL");
        config.challengePeriod = vm.envOr("CHALLENGE_PERIOD", uint256(prev.challengePeriod().raw())).toUint64();
        config.proofPeriod = vm.envOr("PROOF_PERIOD", uint256(prev.proofPeriod().raw())).toUint64();
        config.proposerBond = vm.envOr("PROPOSER_BOND", prev.proposerBond());
        config.challengerBond = vm.envOr("CHALLENGER_BOND", prev.challengerBond());
        config.proofThreshold = vm.envOr("PROOF_THRESHOLD", uint256(prev.PROOF_THRESHOLD())).toUint8();
        config.protocolFeeRecipient = vm.envOr("PROTOCOL_FEE_RECIPIENT", prev.protocolFeeRecipient());
        config.validityProofVerifier =
            IWorldChainProofVerifier(vm.envOr("VALIDITY_PROOF_VERIFIER", address(prev.validityProofVerifier())));
        config.teeVerifier = IWorldChainProofVerifier(vm.envOr("TEE_VERIFIER", address(prev.teeVerifier())));
        config.securityCouncil =
            IWorldChainProofVerifier(vm.envOr("SECURITY_COUNCIL_VERIFIER", address(prev.securityCouncil())));
        config.anchorStateRegistry =
            IAnchorStateRegistry(vm.envOr("ANCHOR_STATE_REGISTRY", address(prev.anchorStateRegistry())));
        config.weth = IDelayedWETH(payable(vm.envOr("DELAYED_WETH", address(prev.weth()))));

        // A fresh DelayedWETH would strand every bond held for in-flight games in the old one.
        require(address(config.weth) == address(prev.weth()), "Upgrade: DelayedWETH must be reused");
        // Lane independence is what makes the threshold mean anything.
        require(
            address(config.validityProofVerifier) != address(config.teeVerifier)
                && address(config.validityProofVerifier) != address(config.securityCouncil)
                && address(config.teeVerifier) != address(config.securityCouncil),
            "Upgrade: proof lane verifiers must be distinct"
        );
    }

    /// @dev The invariants that separate a bytecode swap from a silent fork of the proof domain.
    function _validateReplacement(Config memory config, MultiProofGame newImpl) internal view {
        MultiProofGame prev = config.currentImpl;
        require(address(newImpl) != address(prev), "Upgrade: implementation unchanged");
        require(
            newImpl.domainHash() == prev.domainHash(),
            "Upgrade: domainHash drift invalidates every in-flight proposal and queued proof"
        );
        require(
            address(newImpl.disputeGameFactory()) == address(config.disputeGameFactory),
            "Upgrade: new implementation points at a different factory"
        );
        require(
            address(newImpl.anchorStateRegistry()) == address(prev.anchorStateRegistry()),
            "Upgrade: AnchorStateRegistry changed"
        );
        require(address(newImpl.weth()) == address(prev.weth()), "Upgrade: DelayedWETH changed");
        require(newImpl.PROOF_THRESHOLD() == config.proofThreshold, "Upgrade: proof threshold mismatch");
        require(newImpl.proposerBond() == config.proposerBond, "Upgrade: proposer bond mismatch");
        require(newImpl.challengerBond() == config.challengerBond, "Upgrade: challenger bond mismatch");
        require(
            config.anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw(),
            "Upgrade: game type 1006 is not the respected game type"
        );

        console.log("MultiProofGame upgrade");
        console.log("  previous implementation:", address(prev));
        console.log("  new implementation:     ", address(newImpl));
        console.log("  previous version:       ", prev.version());
        console.log("  new version:            ", newImpl.version());
        console.log("  domainHash (unchanged): ", vm.toString(newImpl.domainHash()));
    }

    function _gameConfig(Config memory config) internal pure returns (IMultiProofGame.GameConfig memory) {
        return IMultiProofGame.GameConfig({
            rollupConfigHash: config.rollupConfigHash,
            blockInterval: config.blockInterval,
            challengePeriod: config.challengePeriod,
            proofPeriod: config.proofPeriod,
            proposerBond: config.proposerBond,
            challengerBond: config.challengerBond,
            protocolFeeRecipient: config.protocolFeeRecipient,
            proofThreshold: config.proofThreshold,
            validityProofVerifier: config.validityProofVerifier,
            teeVerifier: config.teeVerifier,
            securityCouncil: config.securityCouncil,
            anchorStateRegistry: config.anchorStateRegistry,
            weth: config.weth
        });
    }

    /// @dev Patches the two implementation keys in the existing deployment record instead of
    ///      rewriting it, so the untouched addresses cannot drift from what is on chain.
    function _writeDeployment(Config memory config, address newImpl) internal {
        string memory out = vm.envOr("PROOF_SYSTEM_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length == 0) return;

        vm.writeJson(vm.toString(address(config.currentImpl)), out, ".previousGameImplementation");
        vm.writeJson(vm.toString(newImpl), out, ".gameImplementation");
        vm.writeJson(vm.toString(uint256(LibProof.PROOF_SYSTEM_VERSION)), out, ".proofSystemVersion");
    }
}
