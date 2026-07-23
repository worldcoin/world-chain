// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "@forge-std/Test.sol";

import {WorldChainProofSystemGame} from "../../src/proofs/WorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "../../src/proofs/interfaces/IWorldChainStakingRegistry.sol";
import {MockRootIdVerifier} from "../../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../../src/proofs/mocks/MockStakingRegistry.sol";
import {MockSystemConfig} from "../mocks/MockSystemConfig.sol";

import {Claim, GameStatus, GameType, Hash, Proposal} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

/// @dev Test harness deploying the real (pinned) OP `DisputeGameFactory`,
///      `AnchorStateRegistry`, and `DelayedWETH` from the `opstack/` sub-project artifacts,
///      wired to a `WorldChainProofSystemGame` implementation. Run `just build-opstack`
///      before `forge test`.
abstract contract OPStackFixtures is Test {
    GameType internal constant WC_GAME_TYPE = GameType.wrap(42);
    uint256 internal constant FINALITY_DELAY_SECONDS = 3.5 days;
    uint256 internal constant WETH_DELAY_SECONDS = 7 days;
    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;
    uint8 internal constant PROOF_THRESHOLD = 2;

    uint256 internal constant CHAIN_ID = 480;
    uint256 internal constant PROOF_SYSTEM_VERSION = 1;
    bytes32 internal constant ROLLUP_CONFIG_HASH = keccak256("world-chain-rollup-config");
    uint256 internal constant BLOCK_INTERVAL = 100;

    bytes32 internal constant STARTING_ANCHOR_ROOT = keccak256("starting-anchor-root");
    uint256 internal constant STARTING_ANCHOR_BLOCK = 1_000;

    address internal guardian = makeAddr("guardian");
    address internal proposer = makeAddr("proposer");
    address internal challengerAccount = makeAddr("challenger");

    MockSystemConfig internal systemConfig;
    IProxyAdmin internal proxyAdmin;
    IDisputeGameFactory internal dgf;
    IAnchorStateRegistry internal asr;
    IDelayedWETH internal weth;

    MockStakingRegistry internal stakingRegistry;
    MockRootIdVerifier internal validityVerifier;
    MockRootIdVerifier internal teeVerifier;
    MockRootIdVerifier internal councilVerifier;
    WorldChainProofSystemGame internal gameImpl;

    function setUp() public virtual {
        systemConfig = new MockSystemConfig(guardian);

        proxyAdmin = IProxyAdmin(deployCode("opstack/out/ProxyAdmin.sol/ProxyAdmin.json", abi.encode(address(this))));

        // DisputeGameFactory proxy, owned by the test contract.
        dgf = IDisputeGameFactory(_proxied("opstack/out/DisputeGameFactory.sol/DisputeGameFactory.json", ""));
        proxyAdmin.upgradeAndCall(
            payable(address(dgf)), _lastImpl, abi.encodeCall(IDisputeGameFactory.initialize, (address(this)))
        );

        // AnchorStateRegistry proxy.
        asr = IAnchorStateRegistry(
            _proxied("opstack/out/AnchorStateRegistry.sol/AnchorStateRegistry.json", abi.encode(FINALITY_DELAY_SECONDS))
        );
        proxyAdmin.upgradeAndCall(
            payable(address(asr)),
            _lastImpl,
            abi.encodeCall(
                IAnchorStateRegistry.initialize,
                (
                    ISystemConfig(address(systemConfig)),
                    dgf,
                    Proposal({root: Hash.wrap(STARTING_ANCHOR_ROOT), l2SequenceNumber: STARTING_ANCHOR_BLOCK}),
                    WC_GAME_TYPE
                )
            )
        );

        // DelayedWETH proxy.
        weth = IDelayedWETH(
            payable(_proxied("opstack/out/DelayedWETH.sol/DelayedWETH.json", abi.encode(WETH_DELAY_SECONDS)))
        );
        proxyAdmin.upgradeAndCall(
            payable(address(weth)),
            _lastImpl,
            abi.encodeCall(IDelayedWETH.initialize, (ISystemConfig(address(systemConfig))))
        );

        // World Chain proof-system periphery + game implementation.
        stakingRegistry = new MockStakingRegistry();
        stakingRegistry.setStaked(challengerAccount, true);
        validityVerifier = new MockRootIdVerifier(false);
        teeVerifier = new MockRootIdVerifier(false);
        councilVerifier = new MockRootIdVerifier(false);

        gameImpl = new WorldChainProofSystemGame(_gameConfig(WC_GAME_TYPE));
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(gameImpl)));
        dgf.setInitBond(WC_GAME_TYPE, PROPOSER_BOND);

        // The registry retires every game created at or before its initialization timestamp.
        vm.warp(block.timestamp + 1);

        vm.deal(proposer, 100 ether);
        vm.deal(challengerAccount, 100 ether);
    }

    /// @dev Address of the implementation most recently deployed by `_proxied`.
    address internal _lastImpl;

    /// @dev Deploys `artifact` (with `args`) as the implementation behind a fresh OP `Proxy`
    ///      administered by `proxyAdmin`, returning the proxy address.
    function _proxied(string memory artifact, bytes memory args) internal returns (address proxy_) {
        _lastImpl = args.length == 0 ? deployCode(artifact) : deployCode(artifact, args);
        proxy_ = deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(proxyAdmin)));
    }

    function _gameConfig(GameType gameType_) internal view returns (WorldChainProofSystemGame.GameConfig memory) {
        return WorldChainProofSystemGame.GameConfig({
            domain: _domain(),
            gameType: gameType_,
            challengePeriod: CHALLENGE_PERIOD,
            proofPeriod: PROOF_PERIOD,
            proposerBond: PROPOSER_BOND,
            challengerBond: CHALLENGER_BOND,
            proofThreshold: PROOF_THRESHOLD,
            validityProofVerifier: IWorldChainProofVerifier(address(validityVerifier)),
            teeVerifier: IWorldChainProofVerifier(address(teeVerifier)),
            securityCouncil: IWorldChainProofVerifier(address(councilVerifier)),
            stakingRegistry: IWorldChainStakingRegistry(address(stakingRegistry)),
            disputeGameFactory: dgf,
            anchorStateRegistry: asr,
            weth: weth
        });
    }

    function _domain() internal pure returns (WorldChainProofLib.Domain memory) {
        return WorldChainProofLib.Domain({
            chainId: CHAIN_ID,
            proofSystemVersion: PROOF_SYSTEM_VERSION,
            rollupConfigHash: ROLLUP_CONFIG_HASH,
            blockInterval: BLOCK_INTERVAL
        });
    }

    function _extraData(uint256 l2BlockNumber, uint256 parentIndex, uint256 attempt)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(l2BlockNumber, parentIndex, attempt);
    }

    function _rootClaimFor(uint256 l2BlockNumber) internal pure returns (bytes32) {
        return keccak256(abi.encode("output-root", l2BlockNumber));
    }

    /// @dev Creates a game through the stock factory as `proposer`.
    function _propose(uint256 parentIndex, bytes32 rootClaim, uint256 l2BlockNumber, uint256 attempt)
        internal
        returns (WorldChainProofSystemGame)
    {
        vm.prank(proposer);
        IDisputeGame proxy = dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(rootClaim), _extraData(l2BlockNumber, parentIndex, attempt)
        );
        return WorldChainProofSystemGame(address(proxy));
    }

    /// @dev Creates the first game, parented on the current anchor.
    function _proposeAtAnchor() internal returns (WorldChainProofSystemGame) {
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        uint256 target = anchorBlock + BLOCK_INTERVAL;
        return _propose(type(uint256).max, _rootClaimFor(target), target, 0);
    }

    /// @dev Creates a child chained onto the game at factory index `parentIndex`.
    function _proposeChild(uint256 parentIndex) internal returns (WorldChainProofSystemGame) {
        (,, IDisputeGame parent) = dgf.gameAtIndex(parentIndex);
        uint256 target = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        return _propose(parentIndex, _rootClaimFor(target), target, 0);
    }

    function _challenge(WorldChainProofSystemGame game) internal {
        vm.prank(challengerAccount);
        game.challenge{value: CHALLENGER_BOND}();
    }

    /// @dev Submits `laneCount` valid proof lanes; the mock verifiers accept a 32-byte proof
    ///      equal to the game's rootId.
    function _submitLanes(WorldChainProofSystemGame game, uint8 laneCount) internal {
        for (uint8 lane = 0; lane < laneCount; lane++) {
            game.submitProofLane(lane, abi.encodePacked(game.rootId()));
        }
    }

    /// @dev Warps past the challenge window and resolves (unchallenged path).
    function _resolveUnchallenged(WorldChainProofSystemGame game) internal {
        if (block.timestamp < game.challengeDeadline()) {
            vm.warp(game.challengeDeadline());
        }
        game.resolve();
    }

    /// @dev Warps past the registry's finality airgap for a resolved game.
    function _passAirgap(WorldChainProofSystemGame game) internal {
        vm.warp(game.resolvedAt().raw() + FINALITY_DELAY_SECONDS + 1);
    }

    /// @dev Runs the full two-phase DelayedWETH claim for `recipient`.
    function _claim(WorldChainProofSystemGame game, address recipient) internal {
        game.claimCredit(recipient);
        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        game.claimCredit(recipient);
    }
}
