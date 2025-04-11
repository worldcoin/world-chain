// SPDX-License-Identifier: MIT
pragma solidity ^0.8.12;

import "@account-abstraction/contracts/core/BasePaymaster.sol";
import "@account-abstraction/contracts/interfaces/IPaymaster.sol";
import "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import "@openzeppelin/contracts/access/Ownable2Step.sol";

interface IAddressBook {
    function addressVerifiedUntil(address account) external view returns (uint256);
}

contract WorldIdPaymaster is BasePaymaster, Ownable2Step {
    IAddressBook public immutable addressBook;
    
    uint256 public maxDailyTransactions;
    
    // user => date => number of transactions
    mapping(address => mapping(uint256 => uint256)) public userDailyTxs;

    event DailyLimitReached(address indexed sender, uint256 count);
    event MaxDailyTransactionsUpdated(uint256 oldLimit, uint256 newLimit);
    
    constructor(
        IEntryPoint _entryPoint,
        address _addressBook,
        uint256 _maxDailyTransactions
    ) BasePaymaster(_entryPoint) Ownable(msg.sender) {
        require(_maxDailyTransactions > 0, "AddressBookPaymaster: Invalid daily limit");
        addressBook = IAddressBook(_addressBook);
        maxDailyTransactions = _maxDailyTransactions;
    }

    function setMaxDailyTransactions(uint256 newLimit) external onlyOwner {
        require(newLimit > 0, "AddressBookPaymaster: Invalid daily limit");
        uint256 oldLimit = maxDailyTransactions;
        maxDailyTransactions = newLimit;
        emit MaxDailyTransactionsUpdated(oldLimit, newLimit);
    }

    function getDate() public view returns (uint256) {
        return block.timestamp / 86400;
    }

    function _validatePaymasterUserOp(
        UserOperation calldata userOp,
        bytes32 /*userOpHash*/,
        uint256 requiredPreFund
    ) internal view override returns (bytes memory context, uint256 validationData) {
        // Check if sender is verified in address book
        uint256 verifiedUntil = addressBook.addressVerifiedUntil(userOp.sender);
        require(
            verifiedUntil > block.timestamp,
            "AddressBookPaymaster: Sender not verified"
        );

        uint256 today = getDate();
        uint256 txCount = userDailyTxs[userOp.sender][today];
        
        require(
            txCount < maxDailyTransactions,
            "AddressBookPaymaster: Daily transaction limit reached"
        );

        // Just pass the sender, we can get the date again in postOp
        return (abi.encode(userOp.sender), 0);
    }

    function _postOp(
        PostOpMode mode,
        bytes calldata context,
        uint256 actualGasCost
    ) internal override {
        if (mode == PostOpMode.postOpReverted) {
            return;
        }

        address sender = abi.decode(context, (address));
        uint256 today = getDate();
        
        uint256 newCount = userDailyTxs[sender][today] + 1;
        userDailyTxs[sender][today] = newCount;
        
        if (newCount >= maxDailyTransactions) {
            emit DailyLimitReached(sender, newCount);
        }
    }

    function getDailyTransactions(address user) external view returns (uint256) {
        return userDailyTxs[user][getDate()];
    }

    receive() external payable {
        // Accept deposits
    }
}