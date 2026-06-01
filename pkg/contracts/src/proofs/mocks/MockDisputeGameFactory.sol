// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGame} from "../external/IDisputeGame.sol";
import {GameType, Claim, Hash, Timestamp} from "../external/Types.sol";

/// @title MockDisputeGameFactory
/// @notice Test/devnet double for the OP `DisputeGameFactory`. Reproduces the
///         exact `op-contracts/v3.0.0-rc.2` `create()` behavior the World Chain
///         game depends on: clones the implementation with the canonical CWIA
///         payload `abi.encodePacked(msg.sender, rootClaim, parentHash, extraData)`
///         and calls `initialize{value: initBond}()`.
/// @dev The clone-with-immutable-args proxy is the minimal-proxy variant that
///      appends the immutable args to runtime calldata followed by a 2-byte
///      big-endian length suffix — the layout the game's CWIA readers expect.
contract MockDisputeGameFactory {
    mapping(GameType gameType => IDisputeGame impl) public gameImpls;
    mapping(GameType gameType => uint256 bond) public initBonds;
    mapping(Hash uuid => IDisputeGame proxy) internal _games;
    mapping(Hash uuid => Timestamp timestamp) internal _gameTimestamps;

    error NoImplementation(GameType gameType);
    error IncorrectBondAmount();
    error GameAlreadyExists(Hash uuid);

    event DisputeGameCreated(address indexed disputeProxy, GameType indexed gameType, Claim indexed rootClaim);

    function setImplementation(GameType gameType, IDisputeGame impl) external {
        gameImpls[gameType] = impl;
    }

    function setInitBond(GameType gameType, uint256 bond) external {
        initBonds[gameType] = bond;
    }

    function games(GameType gameType, Claim rootClaim, bytes calldata extraData)
        external
        view
        returns (IDisputeGame proxy_, Timestamp timestamp_)
    {
        Hash uuid = getGameUUID(gameType, rootClaim, extraData);
        proxy_ = _games[uuid];
        timestamp_ = _gameTimestamps[uuid];
    }

    function getGameUUID(GameType gameType, Claim rootClaim, bytes calldata extraData)
        public
        pure
        returns (Hash uuid_)
    {
        uuid_ = Hash.wrap(keccak256(abi.encode(gameType, rootClaim, extraData)));
    }

    function create(GameType gameType, Claim rootClaim, bytes calldata extraData)
        external
        payable
        returns (IDisputeGame proxy_)
    {
        IDisputeGame impl = gameImpls[gameType];
        if (address(impl) == address(0)) revert NoImplementation(gameType);
        if (msg.value != initBonds[gameType]) revert IncorrectBondAmount();

        bytes32 parentHash = block.number == 0 ? bytes32(0) : blockhash(block.number - 1);

        // CWIA layout: [0,20) creator, [20,52) rootClaim, [52,84) l1Head, [84,..) extraData.
        bytes memory args = abi.encodePacked(msg.sender, Claim.unwrap(rootClaim), parentHash, extraData);
        proxy_ = IDisputeGame(_clone(address(impl), args));
        proxy_.initialize{value: msg.value}();

        Hash uuid = getGameUUID(gameType, rootClaim, extraData);
        if (address(_games[uuid]) != address(0)) revert GameAlreadyExists(uuid);
        _games[uuid] = proxy_;
        _gameTimestamps[uuid] = Timestamp.wrap(uint64(block.timestamp));

        emit DisputeGameCreated(address(proxy_), gameType, rootClaim);
    }

    /// @notice Deploys a clone-with-immutable-args minimal proxy of `impl` with
    ///         `args` appended to runtime calldata, followed by a 2-byte
    ///         big-endian length suffix.
    /// @dev Verbatim Solady `LibClone.clone(address,bytes)` legacy CWIA runtime
    ///      (the variant op-contracts/v3.0.0-rc.2 relies on — confirmed by
    ///      `FaultDisputeGame.initialize`'s `calldatasize == 0x7A` check whose
    ///      trailing `0x02 CWIA bytes` is this length suffix). Pairs with the
    ///      game's `CWIACloneReader`.
    function _clone(address impl, bytes memory data) internal returns (address instance) {
        assembly {
            let mBefore3 := mload(sub(data, 0x60))
            let mBefore2 := mload(sub(data, 0x40))
            let mBefore1 := mload(sub(data, 0x20))
            let dataLength := mload(data)
            let dataEnd := add(add(data, 0x20), dataLength)
            let mAfter1 := mload(dataEnd)

            let extraLength := add(dataLength, 2)

            mstore(data, 0x5af43d3d93803e606057fd5bf3)
            mstore(sub(data, 0x0d), impl)
            mstore(sub(data, 0x21), or(shl(0x48, extraLength), 0x593da1005b363d3d373d3d3d3d610000806062363936013d73))
            mstore(sub(data, 0x3a), 0x9e4ac34f21c619cefc926c8bd93b54bf5a39c7ab2127a895af1cc0691d7e3dff)
            mstore(
                sub(data, add(0x59, lt(extraLength, 0xff9e))),
                or(shl(0x78, add(extraLength, 0x62)), 0xfd6100003d81600a3d39f336602c57343d527f)
            )
            mstore(dataEnd, shl(0xf0, extraLength))

            instance := create(0, sub(data, 0x4c), add(extraLength, 0x6c))
            if iszero(instance) {
                mstore(0x00, 0x30116425)
                revert(0x1c, 0x04)
            }

            mstore(dataEnd, mAfter1)
            mstore(data, dataLength)
            mstore(sub(data, 0x20), mBefore1)
            mstore(sub(data, 0x40), mBefore2)
            mstore(sub(data, 0x60), mBefore3)
        }
    }
}
