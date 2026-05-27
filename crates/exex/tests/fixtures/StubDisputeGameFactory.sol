// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @notice A minimal stub of the OP `FaultDisputeGame`. Stores just the
///         claimant and root claim — the only fields the OP Proposer reads.
contract StubFaultDisputeGame {
    uint32 public gameType;
    address public claimant;
    bytes32 public claim;

    constructor(uint32 gt, address c, bytes32 cl) payable {
        gameType = gt;
        claimant = c;
        claim = cl;
    }

    /// @notice Layout matches the real `FaultDisputeGame.claimData`:
    ///         (parentIndex, counteredBy, claimant, bond, claim, position, clock)
    function claimData(uint256)
        external
        view
        returns (uint32, address, address, uint128, bytes32, uint128, uint128)
    {
        return (0, address(0), claimant, 0, claim, 0, 0);
    }
}

/// @notice Minimal stub of `DisputeGameFactory`. Tracks created games and
///         lets tests set the per-call bond.
contract StubDisputeGameFactory {
    struct Game {
        uint32 gameType;
        uint64 timestamp;
        address proxy;
    }

    Game[] private games;
    uint256 private bond;

    function setBond(uint256 b) external {
        bond = b;
    }

    function initBonds(uint32) external view returns (uint256) {
        return bond;
    }

    function gameCount() external view returns (uint256) {
        return games.length;
    }

    function gameAtIndex(uint256 i)
        external
        view
        returns (uint32 gameType_, uint64 timestamp_, address proxy_)
    {
        Game memory g = games[i];
        return (g.gameType, g.timestamp, g.proxy);
    }

    function version() external pure returns (string memory) {
        return "stub-1.0";
    }

    function create(uint32 gt, bytes32 root, bytes calldata) external payable returns (address) {
        require(msg.value == bond, "bad bond");
        StubFaultDisputeGame g = new StubFaultDisputeGame(gt, msg.sender, root);
        games.push(Game(gt, uint64(block.timestamp), address(g)));
        return address(g);
    }
}
