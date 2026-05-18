// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title FeeRecipient
/// @author Worldcoin
/// @notice Receives ETH and splits it between a burn address and a fee vault
/// @dev On ETH receipt, sends `distributionRatio` portion to BURN_RECIPIENT, keeps the rest
contract FeeRecipient is Ownable {
    using SafeERC20 for IERC20;

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Scaling factor for ratio calculations (100000 = 100%)
    uint24 internal constant SCALE = 100000;

    /// @notice Address that receives the burn portion of incoming ETH
    address public immutable FEE_BURN_ESCROW;

    /// @notice Address that receives withdrawn funds
    address public feeVaultRecipient;

    /// @notice Ratio of incoming ETH sent to burn (in basis points, SCALE = 100000)
    uint24 public distributionRatio = 50000;

    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when ETH transfer fails
    error ETHTransferFailed();

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor(address _feeBurn, address _recipient, uint24 _ratio, address _owner) Ownable(_owner) {
        FEE_BURN_ESCROW = _feeBurn;
        feeVaultRecipient = _recipient;
        distributionRatio = _ratio;
    }

    /*//////////////////////////////////////////////////////////////
                               FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /// @notice Withdraws all ETH balance to FEE_VAULT_RECIPIENT
    function withdraw() external onlyOwner {
        uint256 balance = address(this).balance;
        safeTransferETH(feeVaultRecipient, balance);
    }

    /// @notice Withdraws all of a token to FEE_VAULT_RECIPIENT
    /// @param token The ERC20 token to withdraw
    function withdraw(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        IERC20(token).safeTransfer(feeVaultRecipient, balance);
    }

    /// @dev Sends burn portion of msg.value to BURN_RECIPIENT
    function distribute() private {
        uint256 burn = (msg.value * distributionRatio) / SCALE;
        safeTransferETH(FEE_BURN_ESCROW, burn);
    }

    /// @dev Transfers ETH using assembly for gas efficiency
    function safeTransferETH(address to, uint256 amount) private {
        bool success;
        /// @solidity memory-safe-assembly
        assembly {
            success := call(gas(), to, amount, 0, 0, 0, 0)
        }

        if (!success) {
            revert ETHTransferFailed();
        }
    }

    receive() external payable {
        distribute();
    }

    fallback() external payable {
        distribute();
    }
}
