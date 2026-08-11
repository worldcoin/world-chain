// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWldEthOracle} from "../../src/interfaces/IWldEthOracle.sol";
import {IAggregatorV3} from "../../src/interfaces/IAggregatorV3.sol";
import {IAccount} from "@account-abstraction/interfaces/IAccount.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {ISwapRouter, IWETH9} from "../../src/interfaces/ISwapRouter.sol";

/// @notice Simple mintable ERC20 used for WLD in tests.
contract MockERC20 is ERC20 {
    constructor(string memory n, string memory s) ERC20(n, s) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @notice Minimal WETH9 that backs withdrawals with real ETH.
contract MockWETH is IWETH9 {
    string public constant name = "Wrapped Ether";
    mapping(address => uint256) public balanceOf;

    function depositTo(address to) external payable {
        balanceOf[to] += msg.value;
    }

    function withdraw(uint256 wad) external override {
        require(balanceOf[msg.sender] >= wad, "WETH: insufficient");
        balanceOf[msg.sender] -= wad;
        (bool ok,) = payable(msg.sender).call{value: wad}("");
        require(ok, "WETH: eth send failed");
    }

    receive() external payable {
        balanceOf[msg.sender] += msg.value;
    }
}

/**
 * @notice Configurable WLD/ETH oracle.
 * @dev wldForEth(ethWei) = ethWei * num / den ; ethForWld is the inverse.
 *      Set `stale=true` to simulate an unusable oracle (reverts).
 */
contract MockOracle is IWldEthOracle {
    uint256 public num; // WLD per ETH numerator
    uint256 public den; // denominator
    bool public stale;

    constructor(uint256 _num, uint256 _den) {
        num = _num;
        den = _den;
    }

    function setRate(uint256 _num, uint256 _den) external {
        num = _num;
        den = _den;
    }

    function setStale(bool _stale) external {
        stale = _stale;
    }

    function wldForEth(uint256 ethWei) external view override returns (uint256) {
        require(!stale, "OLD");
        return (ethWei * num) / den;
    }

    function ethForWld(uint256 wldAmount) external view override returns (uint256) {
        require(!stale, "OLD");
        return (wldAmount * den) / num;
    }
}

/**
 * @notice Mock Uniswap V3 SwapRouter: converts WLD -> WETH at `num/den` with an
 *         optional `slippageBps` haircut to simulate real execution price.
 * @dev Must be pre-funded with ETH so it can mint backed WETH to the recipient.
 */
contract MockSwapRouter is ISwapRouter {
    MockWETH public immutable weth;
    uint256 public num; // ETH out per WLD numerator (mirror of oracle inverse)
    uint256 public den;
    uint256 public slippageBps; // haircut applied to output

    constructor(MockWETH _weth, uint256 _num, uint256 _den) {
        weth = _weth;
        num = _num;
        den = _den;
    }

    function setSlippageBps(uint256 _bps) external {
        slippageBps = _bps;
    }

    receive() external payable {}

    function exactInputSingle(ExactInputSingleParams calldata p) external payable override returns (uint256 amountOut) {
        IERC20(p.tokenIn).transferFrom(msg.sender, address(this), p.amountIn);
        // ETH out = amountIn * den / num  (since num/den is WLD-per-ETH)
        uint256 ideal = (p.amountIn * den) / num;
        amountOut = (ideal * (10_000 - slippageBps)) / 10_000;
        require(amountOut >= p.amountOutMinimum, "Too little received");
        weth.depositTo{value: amountOut}(p.recipient);
    }
}

/// @notice Mock Uniswap V3 pool with a settable spot price, for the paymaster's
///         swap deviation guard.
contract MockUniswapV3Pool {
    address public immutable token0;
    address public immutable token1;
    uint24 public constant fee = 3000;
    uint160 public sqrtPriceX96;

    constructor(address _token0, address _token1, uint160 _sqrtPriceX96) {
        // Uniswap orders token0 < token1
        (token0, token1) = _token0 < _token1 ? (_token0, _token1) : (_token1, _token0);
        sqrtPriceX96 = _sqrtPriceX96;
    }

    function setSqrtPriceX96(uint160 _sqrtPriceX96) external {
        sqrtPriceX96 = _sqrtPriceX96;
    }

    function slot0() external view returns (uint160, int24, uint16, uint16, uint16, uint8, bool) {
        return (sqrtPriceX96, 0, 0, 1, 1, 0, true);
    }
}

/**
 * @notice Mock Chainlink AggregatorV3 feed.
 * @dev Mirrors the World Chain `ChainlinkPriceFeed` read surface: configurable
 *      `decimals`, plus setters to simulate stale, non-positive, and unset rounds.
 */
contract MockAggregatorV3 is IAggregatorV3 {
    uint8 public immutable override decimals;
    string public override description;
    uint256 public constant override version = 1;

    int256 public answer;
    uint256 public updatedAt;
    uint80 public roundId = 1;
    /// @dev When true, `latestRoundData` reverts (feed contract itself failing).
    bool public reverting;

    constructor(string memory _description, uint8 _decimals, int256 _answer) {
        description = _description;
        decimals = _decimals;
        answer = _answer;
        updatedAt = block.timestamp;
    }

    function setAnswer(int256 _answer) external {
        answer = _answer;
        updatedAt = block.timestamp;
        roundId += 1;
    }

    function setUpdatedAt(uint256 _updatedAt) external {
        updatedAt = _updatedAt;
    }

    function setReverting(bool _v) external {
        reverting = _v;
    }

    function latestRoundData() external view override returns (uint80, int256, uint256, uint256, uint80) {
        require(!reverting, "feed down");
        return (roundId, answer, updatedAt, updatedAt, roundId);
    }

    function getRoundData(uint80 _roundId) external view override returns (uint80, int256, uint256, uint256, uint80) {
        require(!reverting, "feed down");
        return (_roundId, answer, updatedAt, updatedAt, _roundId);
    }
}

    /**
     * @notice Minimal ERC-4337 account that always validates.
     * @dev Lets tests drive real `EntryPoint.handleOps` so ordering-sensitive behaviour
     *      is exercised — notably that the EntryPoint deducts the paymaster's prefund
     *      *before* calling `validatePaymasterUserOp`. Implements {IAccount} rather than
     *      a hand-rolled signature so the selector actually matches what the EntryPoint
     *      calls (a `bytes` param instead of the struct silently hits `fallback`).
     */
    contract MockAccount is IAccount {
        function validateUserOp(PackedUserOperation calldata, bytes32, uint256)
            external
            pure
            override
            returns (uint256)
        {
            return 0; // valid, no time range
        }

        fallback() external payable {}
        receive() external payable {}
    }
