// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Hash, Proposal} from "../DisputeTypes.sol";
import {IOptimismPortal2AnchorStateRegistry} from "./IOptimismPortal2.sol";

interface IWorldChainAnchorStateRegistry is IOptimismPortal2AnchorStateRegistry {
    function setAnchorState(address game) external;
    function isGameFinalized(address game) external view returns (bool);
    function isGameRegistered(address game) external view returns (bool);
    function getAnchorRoot() external view returns (Hash, uint256);
    function getStartingAnchorRoot() external view returns (Proposal memory);
    function paused() external view returns (bool);
    function anchorGame() external view returns (address);
    function isGameBlacklisted(address game) external view returns (bool);
}
