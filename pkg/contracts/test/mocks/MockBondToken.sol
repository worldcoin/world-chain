// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract MockBondToken is ERC20 {
    constructor() ERC20("Bond Token", "BOND") {}

    function mint(address recipient, uint256 amount) external {
        _mint(recipient, amount);
    }
}
