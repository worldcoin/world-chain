// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

contract LoadTestContract {
    mapping(uint256 => bool) public map;
    uint256 public nonce;

    function sstore() external {
        for (uint256 i = 0; i < 100; i++) {
            nonce += 1;
            bool value = map[nonce];
            map[nonce] = !value;
        }
    }
}

contract StorageTest {
        mapping(uint256 => uint256) private slots;

        /// Sets storage at `slot` to `value`
        function setSlot(uint256 slot, uint256 value) external returns (uint256) {
            slots[slot] = value;
            return value;
        }

        /// Gets storage at `slot`
        function getSlot(uint256 slot) external view returns (uint256) {
            return slots[slot];
        }

        /// Reads slot, increments it, and returns the original value
        function getAndIncrement(uint256 slot) external returns (uint256) {
            uint256 current = slots[slot];
            slots[slot] = current + 1;
            return current;
        }
    }