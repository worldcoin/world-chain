// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

contract LoadTestContract {
        mapping(uint256 => bool) public map;
        uint public nonce;

        function sstore() external {
            for (uint256 i = 0; i < 1000; i++) {
                nonce += 1;
                bool value = map[nonce];
                map[nonce] = !value;
            }
        }
    }