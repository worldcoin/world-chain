// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// ChaosTest - Main logic contract with various state-modifying functions
/// designed to create overlapping BAL dependencies between transactions.
contract ChaosTest {
    constructor() {}

    bytes32 constant FIB_BASE =
        0x0000000000000000000000000000000000000000000000000000000000000001;

    function fib(uint256 n) public returns (uint256 result) {
        assembly {
            sstore(FIB_BASE, 0)
            sstore(add(FIB_BASE, 1), 1)
            switch n
            case 0 {
                result := 0
            }
            case 1 {
                result := 1
            }
            default {
                for {
                    let i := 2
                } lt(i, add(n, 1)) {
                    i := add(i, 1)
                } {
                    let fib_i_minus_1 := sload(add(FIB_BASE, sub(i, 1)))
                    let fib_i_minus_2 := sload(add(FIB_BASE, sub(i, 2)))
                    let fib_i := add(fib_i_minus_1, fib_i_minus_2)
                    sstore(add(FIB_BASE, i), fib_i)
                }
                result := sload(add(FIB_BASE, n))
            }
        }
    }
}

contract ChaosTestProxy {
    bytes32 constant IMPL_SLOT =
        0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;
    constructor(address _implementation) {
        assembly {
            sstore(IMPL_SLOT, _implementation)
        }
    }
    function deployNewImplementation() external {
        ChaosTest newImpl = new ChaosTest();
        assembly {
            sstore(IMPL_SLOT, newImpl)
        }
    }
    fallback() external payable {
        assembly {
            let impl := sload(IMPL_SLOT)
            calldatacopy(0x00, 0x00, calldatasize())

            let success := delegatecall(
                gas(),
                impl,
                0x00,
                calldatasize(),
                0x00,
                0x00
            )

            returndatacopy(0x00, 0x00, returndatasize())

            switch success
            case 0 {
                revert(0x00, returndatasize())
            }
            default {
                return(0x00, returndatasize())
            }
        }
    }
    receive() external payable {}
}
