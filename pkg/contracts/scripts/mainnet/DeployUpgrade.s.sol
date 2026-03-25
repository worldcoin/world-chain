// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {PBHEntryPointImplV1} from "../../src/pbh/PBHEntryPointImplV1.sol";
import {console} from "forge-std/console.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {Create2Factory} from "./Create2Deploy.sol";

contract DeployUpgrade is Create2Factory, Script {
    address public pbhEntryPointImpl;

    UUPSUpgradeable immutable PBH_ENTRY_POINT = UUPSUpgradeable(0x0000000000A21818Ee9F93BB4f2AAad305b5397C);

    function run() public {
        console.log(
            "Deploying: PBHEntryPointImplV1"
        );
        bytes32 implSalt = vm.envBytes32("IMPL_SALT");
        uint256 privateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(privateKey);
        deployPBHEntryPoint(implSalt);
        vm.stopBroadcast();
    }

    function deployPBHEntryPoint(bytes32 implSalt) public {
        pbhEntryPointImpl = deploy(
            implSalt,
            type(PBHEntryPointImplV1).creationCode
        );
        console.log("PBHEntryPointImplV1 Deployed at: ", pbhEntryPointImpl);

        PBH_ENTRY_POINT.upgradeToAndCall(address(pbhEntryPointImpl), hex"");
    }
}
