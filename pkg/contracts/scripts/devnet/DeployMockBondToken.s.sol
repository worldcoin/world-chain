// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {MockBondToken} from "../../test/mocks/MockBondToken.sol";

/// @notice Deploys the mintable ERC-20 bond token used by a devnet with real proof verifiers.
/// @dev Devnet only. This is separate from DeployProofMocks so deployments do not create
///      accept-any verifier contracts when only a mock bond token is needed.
contract DeployMockBondToken is Script {
    function run() external returns (MockBondToken bondToken) {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(privateKey);
        bondToken = new MockBondToken();
        vm.stopBroadcast();

        string memory out = vm.envOr("MOCK_BOND_TOKEN_DEPLOYMENT_OUT", string(""));
        if (bytes(out).length != 0) {
            string memory json = vm.serializeAddress("mockBondToken", "bondToken", address(bondToken));
            vm.writeJson(json, out);
        }
    }
}
