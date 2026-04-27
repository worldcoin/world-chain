// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

import {Test} from "forge-std/Test.sol";
import {FeeRecipient} from "../src/fees/FeeRecipient.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract MockERC20 is ERC20 {
    constructor() ERC20("Mock", "MCK") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

contract TestFeeRecipient is Test {
    FeeRecipient recipient;
    MockERC20 token;

    address owner;
    address burnRecipient;
    address feeVaultRecipient;

    uint24 constant RATIO = 50000; // 50%
    uint24 constant SCALE = 100000;

    function setUp() public {
        owner = makeAddr("owner");
        burnRecipient = makeAddr("burnRecipient");
        feeVaultRecipient = makeAddr("feeVaultRecipient");

        recipient = new FeeRecipient(burnRecipient, feeVaultRecipient, RATIO, owner);

        token = new MockERC20();
    }

    function test_Constructor() public view {
        assertEq(recipient.FEE_BURN_ESCROW(), burnRecipient);
        assertEq(recipient.feeVaultRecipient(), feeVaultRecipient);
        assertEq(recipient.distributionRatio(), RATIO);
        assertEq(recipient.owner(), owner);
    }

    function test_Receive_DistributesETH(uint96 amount) public {
        vm.assume(amount > 0);

        uint256 expectedBurn = (uint256(amount) * RATIO) / SCALE;
        uint256 expectedKept = amount - expectedBurn;

        (bool success,) = address(recipient).call{value: amount}("");
        assertTrue(success);

        assertEq(burnRecipient.balance, expectedBurn);
        assertEq(address(recipient).balance, expectedKept);
    }

    function test_Fallback_DistributesETH(uint96 amount) public {
        vm.assume(amount > 0);

        uint256 expectedBurn = (uint256(amount) * RATIO) / SCALE;
        uint256 expectedKept = amount - expectedBurn;

        (bool success,) = address(recipient).call{value: amount}(abi.encodeWithSignature("nonExistentFunction()"));
        assertTrue(success);

        assertEq(burnRecipient.balance, expectedBurn);
        assertEq(address(recipient).balance, expectedKept);
    }

    function test_WithdrawETH(uint96 amount) public {
        vm.assume(amount > 0);

        vm.deal(address(recipient), amount);

        vm.prank(owner);
        recipient.withdraw();

        assertEq(feeVaultRecipient.balance, amount);
        assertEq(address(recipient).balance, 0);
    }

    function test_WithdrawETH_RevertsIfNotOwner(address caller) public {
        vm.assume(caller != owner);

        vm.deal(address(recipient), 1 ether);

        vm.prank(caller);
        vm.expectRevert();
        recipient.withdraw();
    }

    function test_WithdrawToken(uint128 amount) public {
        vm.assume(amount > 0);

        token.mint(address(recipient), amount);

        vm.prank(owner);
        recipient.withdraw(address(token));

        assertEq(token.balanceOf(feeVaultRecipient), amount);
        assertEq(token.balanceOf(address(recipient)), 0);
    }

    function test_WithdrawToken_RevertsIfNotOwner(address caller) public {
        vm.assume(caller != owner);

        token.mint(address(recipient), 100);

        vm.prank(caller);
        vm.expectRevert();
        recipient.withdraw(address(token));
    }

    function test_Distribute_CustomRatio(uint24 ratio, uint96 amount) public {
        vm.assume(ratio <= SCALE);
        vm.assume(amount > 0);

        FeeRecipient customRecipient = new FeeRecipient(burnRecipient, feeVaultRecipient, ratio, owner);

        uint256 burnBefore = burnRecipient.balance;
        uint256 expectedBurn = (uint256(amount) * ratio) / SCALE;

        (bool success,) = address(customRecipient).call{value: amount}("");
        assertTrue(success);

        assertEq(burnRecipient.balance - burnBefore, expectedBurn);
    }

    receive() external payable {}
}
