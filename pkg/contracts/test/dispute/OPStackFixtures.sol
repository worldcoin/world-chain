// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "@forge-std/Test.sol";

import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {WLDStakingVault} from "../../src/dispute/WLDStakingVault.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";
import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";
import {LibProof, ProofLane} from "../../src/dispute/lib/LibProof.sol";
import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";
import {MockRootIdVerifier} from "../mocks/MockRootIdVerifier.sol";
import {MockSystemConfig} from "../mocks/MockSystemConfig.sol";
import {MockWLD} from "../mocks/MockWLD.sol";

import {Claim, GameStatus, GameType, Hash, Proposal} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

/// @dev Test harness deploying the real (pinned) OP `DisputeGameFactory`,
///      `AnchorStateRegistry`, and `Proxy` from the `opstack/` sub-project artifacts,
///      wired to a `MultiProofGame` implementation. Run `just build-opstack`
///      before `forge test`.
abstract contract OPStackFixtures is Test {
    GameType internal constant WC_GAME_TYPE = GameTypes.MULTI_PROOF_GAME_TYPE;
    uint256 internal constant FINALITY_DELAY_SECONDS = 3.5 days;
    uint256 internal constant WLD_WITHDRAWAL_DELAY_SECONDS = 7 days;
    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant WLD_UNIT = 1e18;
    uint256 internal constant PROPOSER_BOND = WLD_UNIT;
    uint256 internal constant CHALLENGER_BOND = WLD_UNIT;
    uint8 internal constant PROOF_THRESHOLD = 2;

    uint256 internal constant CHAIN_ID = 480;
    bytes32 internal constant ROLLUP_CONFIG_HASH = keccak256("world-chain-rollup-config");
    bytes32 internal constant AGGREGATION_VKEY = keccak256("aggregation-vkey");
    bytes32 internal constant RANGE_VKEY_COMMITMENT = keccak256("range-vkey-commitment");
    bytes32 internal constant TEE_IMAGE_ID = keccak256("tee-image-id");
    uint256 internal constant BLOCK_INTERVAL = 100;

    bytes32 internal constant STARTING_ANCHOR_ROOT = keccak256("starting-anchor-root");
    uint256 internal constant STARTING_ANCHOR_BLOCK = 1_000;

    address internal guardian = makeAddr("guardian");
    address internal proposer = makeAddr("proposer");
    address internal challengerAccount = makeAddr("challenger");
    address internal creationKeeper = makeAddr("creation-keeper");
    address internal protocolFeeRecipient = makeAddr("protocol-fee-recipient");

    MockSystemConfig internal systemConfig;
    IProxyAdmin internal proxyAdmin;
    IDisputeGameFactory internal dgf;
    IAnchorStateRegistry internal asr;
    MockWLD internal wld;
    IWLDStakingVault internal bondVault;

    MockRootIdVerifier internal validityVerifier;
    MockRootIdVerifier internal teeVerifier;
    MockRootIdVerifier internal councilVerifier;
    MultiProofGame internal gameImpl;

    function setUp() public virtual {
        systemConfig = new MockSystemConfig(guardian, CHAIN_ID);

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

        // WIP-1006 WLD staking vault proxy.
        wld = new MockWLD();
        WLDStakingVault vaultImpl = new WLDStakingVault(WLD_WITHDRAWAL_DELAY_SECONDS);
        bondVault = IWLDStakingVault(deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(proxyAdmin))));
        proxyAdmin.upgradeAndCall(
            payable(address(bondVault)),
            address(vaultImpl),
            abi.encodeCall(IWLDStakingVault.initialize, (wld, ISystemConfig(address(systemConfig)), dgf))
        );

        // World Chain proof-system periphery + game implementation.
        validityVerifier = new MockRootIdVerifier(false);
        teeVerifier = new MockRootIdVerifier(false);
        councilVerifier = new MockRootIdVerifier(false);

        gameImpl = new MultiProofGame(_gameConfig());
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(gameImpl)), hex"");
        dgf.setInitBond(WC_GAME_TYPE, 0);

        // The registry retires every game created at or before its initialization timestamp.
        vm.warp(block.timestamp + 1);

        _depositWLD(proposer, 100 * WLD_UNIT);
        _depositWLD(challengerAccount, 100 * WLD_UNIT);
    }

    /// @dev Address of the implementation most recently deployed by `_proxied`.
    address internal _lastImpl;

    /// @dev Deploys `artifact` (with `args`) as the implementation behind a fresh OP `Proxy`
    ///      administered by `proxyAdmin`, returning the proxy address.
    function _proxied(string memory artifact, bytes memory args) internal returns (address proxy_) {
        _lastImpl = args.length == 0 ? deployCode(artifact) : deployCode(artifact, args);
        proxy_ = deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(proxyAdmin)));
    }

    function _gameConfig() internal view returns (IMultiProofGame.GameConfig memory) {
        return IMultiProofGame.GameConfig({
            rollupConfigHash: ROLLUP_CONFIG_HASH,
            blockInterval: BLOCK_INTERVAL,
            challengePeriod: CHALLENGE_PERIOD,
            proofPeriod: PROOF_PERIOD,
            proposerBond: PROPOSER_BOND,
            challengerBond: CHALLENGER_BOND,
            protocolFeeRecipient: protocolFeeRecipient,
            proofThreshold: PROOF_THRESHOLD,
            aggregationVKey: AGGREGATION_VKEY,
            rangeVKeyCommitment: RANGE_VKEY_COMMITMENT,
            teeImageId: TEE_IMAGE_ID,
            validityProofVerifier: IWorldChainProofVerifier(address(validityVerifier)),
            teeVerifier: IWorldChainProofVerifier(address(teeVerifier)),
            securityCouncil: IWorldChainProofVerifier(address(councilVerifier)),
            anchorStateRegistry: asr,
            bondVault: bondVault
        });
    }

    function _domainHash() internal pure returns (bytes32) {
        return LibProof.domainHash(CHAIN_ID, LibProof.PROOF_SYSTEM_VERSION, ROLLUP_CONFIG_HASH, BLOCK_INTERVAL);
    }

    function _extraData(uint256 l2BlockNumber, uint256 parentIndex, uint256 attempt)
        internal
        view
        returns (bytes memory)
    {
        address parent = address(asr);
        if (parentIndex != type(uint256).max) {
            (,, IDisputeGame parentGame) = dgf.gameAtIndex(parentIndex);
            parent = address(parentGame);
        }
        return _extraDataForParent(l2BlockNumber, parent, attempt);
    }

    function _extraDataForParent(uint256 l2BlockNumber, address parent, uint256 attempt)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(_domainHash(), l2BlockNumber, parent, attempt);
    }

    function _rootClaimFor(uint256 l2BlockNumber) internal pure returns (bytes32) {
        return keccak256(abi.encode("output-root", l2BlockNumber));
    }

    function _depositWLD(address account, uint256 amount) internal {
        wld.mint(account, amount);
        vm.startPrank(account);
        wld.approve(address(bondVault), amount);
        bondVault.deposit(amount);
        vm.stopPrank();
    }

    function _reserve(address account, Claim rootClaim, bytes memory extraData) internal {
        vm.prank(account);
        bondVault.reserveProposal(rootClaim, extraData);
    }

    /// @dev Reserves as `proposer`, then creates through the stock factory as a keeper.
    function _propose(uint256 parentIndex, bytes32 rootClaim, uint256 l2BlockNumber, uint256 attempt)
        internal
        returns (MultiProofGame)
    {
        bytes memory extraData = _extraData(l2BlockNumber, parentIndex, attempt);
        Claim claim = Claim.wrap(rootClaim);
        _reserve(proposer, claim, extraData);
        vm.prank(creationKeeper);
        IDisputeGame proxy = dgf.create(WC_GAME_TYPE, claim, extraData);
        return MultiProofGame(address(proxy));
    }

    /// @dev Creates the first game, parented on the current anchor.
    function _proposeAtAnchor() internal returns (MultiProofGame) {
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        uint256 target = anchorBlock + BLOCK_INTERVAL;
        IDisputeGame anchorGame = asr.anchorGame();
        address parent = address(anchorGame) == address(0) ? address(asr) : address(anchorGame);
        bytes memory extraData = _extraDataForParent(target, parent, 0);
        Claim claim = Claim.wrap(_rootClaimFor(target));
        _reserve(proposer, claim, extraData);
        vm.prank(creationKeeper);
        IDisputeGame proxy = dgf.create(WC_GAME_TYPE, claim, extraData);
        return MultiProofGame(address(proxy));
    }

    /// @dev Creates a child chained onto the game at factory index `parentIndex`.
    function _proposeChild(uint256 parentIndex) internal returns (MultiProofGame) {
        (,, IDisputeGame parent) = dgf.gameAtIndex(parentIndex);
        uint256 target = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        return _propose(parentIndex, _rootClaimFor(target), target, 0);
    }

    function _challenge(MultiProofGame game) internal {
        vm.prank(challengerAccount);
        game.challenge();
    }

    /// @dev Packs a compact `submitProofLane` payload: lane id, reward recipient, then the proof.
    function _compact(uint8 laneId, address recipient, bytes memory proof) internal pure returns (bytes memory) {
        return abi.encodePacked(laneId, recipient, proof);
    }

    /// @dev Submits `laneCount` valid proof lanes; the mock verifiers accept a 32-byte proof
    ///      equal to the game's rootId. Each lane names a distinct recipient so reward splits
    ///      are attributable: `laneRewardRecipient(lane)`.
    function _submitLanes(MultiProofGame game, uint8 laneCount) internal {
        bytes memory proof = abi.encodePacked(game.rootId());
        for (uint8 lane = 0; lane < LibProof.PROOF_LANE_COUNT; lane++) {
            if (game.proofBitmap().count() >= laneCount) return;
            game.submitProofLane(_compact(lane, laneRewardRecipient(lane), proof));
        }
    }

    /// @dev Deterministic per-lane reward recipient used by the submit helpers.
    function laneRewardRecipient(uint8 laneId) internal pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked("lane-recipient", laneId)))));
    }

    /// @dev Warps past the challenge window and resolves (unchallenged path).
    function _resolveUnchallenged(MultiProofGame game) internal {
        if (game.proofBitmap().raw() == 0) {
            uint8 lane = uint8(ProofLane.TEE_ATTESTATION);
            game.submitProofLane(_compact(lane, laneRewardRecipient(lane), abi.encodePacked(game.rootId())));
        }
        if (block.timestamp < game.challengeDeadline().raw()) {
            vm.warp(game.challengeDeadline().raw());
        }
        game.resolve();
    }

    /// @dev Warps past the registry's finality airgap for a resolved game.
    function _passAirgap(MultiProofGame game) internal {
        vm.warp(game.resolvedAt().raw() + FINALITY_DELAY_SECONDS + 1);
    }

    /// @dev Closes the game, then runs the vault's delayed external WLD withdrawal.
    function _claim(MultiProofGame game, address recipient) internal {
        game.closeGame();
        uint256 available = bondVault.availableBalance(recipient);
        vm.prank(recipient);
        bondVault.requestWithdrawal(available);
        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS);
        (uint256 amount,) = bondVault.withdrawals(recipient);
        vm.prank(recipient);
        bondVault.withdraw(amount);
    }
}
