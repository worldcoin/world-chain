// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";
import {console} from "forge-std/console.sol";

/// @notice Shared EOA / 2-of-2 Safe execution for devnet administration.
abstract contract Governance is Script {
    function _safeMode() internal view virtual returns (bool) {
        bytes32 mode = keccak256(bytes(vm.envOr("GOVERNANCE_MODE", string("eoa"))));
        require(mode == keccak256("eoa") || mode == keccak256("safe"), "Governance: invalid mode");
        return mode == keccak256("safe");
    }

    function _governanceAddress(uint256 eoaKey) internal view returns (address) {
        if (_safeMode()) return address(_governanceSafe());
        require(eoaKey != 0, "Governance: EOA key required");
        return vm.addr(eoaKey);
    }

    function _governanceSafe() internal view returns (Safe account) {
        account = Safe(payable(_safeAddress()));
        _validateSafe(account);
    }

    function _validateSafe(Safe account) internal view {
        require(address(account).code.length > 0, "Governance: Safe has no code");
        require(account.getThreshold() == 2 && account.getOwners().length == 2, "Governance: expected 2-of-2 Safe");
        (address[] memory modules,) = account.getModulesPaginated(address(1), 1);
        require(modules.length == 0, "Governance: modules bypass threshold");
        bytes memory message = abi.encode(keccak256("devnet council configuration check"));
        bytes32 expected = keccak256(
            abi.encodePacked(
                hex"1901",
                account.domainSeparator(),
                keccak256(abi.encode(keccak256("SafeMessage(bytes message)"), keccak256(message)))
            )
        );
        (bool success, bytes memory result) =
            address(account).staticcall(abi.encodeWithSignature("getMessageHash(bytes)", message));
        require(
            success && result.length == 32 && abi.decode(result, (bytes32)) == expected,
            "Governance: compatibility handler required"
        );
    }

    function _safeAddress() internal view virtual returns (address) {
        return vm.envOr("GOVERNANCE_SAFE", address(0));
    }

    function _signerKeys() internal view virtual returns (uint256, uint256) {
        return (vm.envUint("GOVERNANCE_SIGNER_1_PRIVATE_KEY"), vm.envUint("GOVERNANCE_SIGNER_2_PRIVATE_KEY"));
    }

    function _relayerKey() internal view virtual returns (uint256) {
        return vm.envUint("PRIVATE_KEY");
    }

    function _safeTxOut() internal view virtual returns (string memory) {
        return vm.envOr("SAFE_TX_OUT", string(""));
    }

    function _sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _safeSignatures(bytes32 digest) internal view returns (bytes memory) {
        (uint256 firstKey, uint256 secondKey) = _signerKeys();
        require(firstKey != 0 && secondKey != 0 && firstKey != secondKey, "Governance: two distinct keys required");
        Safe account = _governanceSafe();
        require(
            account.isOwner(vm.addr(firstKey)) && account.isOwner(vm.addr(secondKey)), "Governance: signer mismatch"
        );
        if (vm.addr(firstKey) > vm.addr(secondKey)) (firstKey, secondKey) = (secondKey, firstKey);
        return bytes.concat(_sign(firstKey, digest), _sign(secondKey, digest));
    }

    function _preparingSafe() internal view returns (bool) {
        return _safeMode() && bytes(_safeTxOut()).length != 0;
    }

    string private preparedTransactions;

    function _prepareCall(address target, bytes memory data) internal {
        _governanceSafe();
        if (bytes(preparedTransactions).length != 0) preparedTransactions = string.concat(preparedTransactions, ",");
        preparedTransactions = string.concat(
            preparedTransactions,
            '{"to":"',
            vm.toString(target),
            '","value":"0","data":"',
            vm.toString(data),
            '","contractMethod":null,"contractInputsValues":null}'
        );
        string memory out = _safeTxOut();
        vm.writeFile(
            out,
            string.concat(
                '{"version":"1.0","chainId":"',
                vm.toString(block.chainid),
                '","createdAt":0,"meta":{"name":"Devnet governance","createdFromSafeAddress":"',
                vm.toString(_safeAddress()),
                '"},"transactions":[',
                preparedTransactions,
                "]}"
            )
        );
        console.log("Safe approval required; import into Transaction Builder:", out);
    }

    function _executeGovernance(uint256 eoaKey, address target, bytes memory data) internal {
        require(target.code.length > 0, "Governance: target has no code");
        if (!_safeMode()) {
            require(eoaKey != 0, "Governance: EOA key required");
            vm.startBroadcast(eoaKey);
            (bool success, bytes memory result) = target.call(data);
            vm.stopBroadcast();
            if (!success) {
                assembly ("memory-safe") { revert(add(result, 32), mload(result)) }
            }
            return;
        }
        if (_preparingSafe()) {
            _prepareCall(target, data);
            return;
        }
        Safe account = _governanceSafe();
        bytes32 digest = account.getTransactionHash(
            target, 0, data, Enum.Operation.Call, 0, 0, 0, address(0), address(0), account.nonce()
        );
        bytes memory signatures = _safeSignatures(digest);
        vm.startBroadcast(_relayerKey());
        bool safeSuccess = account.execTransaction(
            target, 0, data, Enum.Operation.Call, 0, 0, 0, address(0), payable(address(0)), signatures
        );
        vm.stopBroadcast();
        require(safeSuccess, "Governance: Safe execution failed");
    }
}

/// @notice Execute one administrative call; PRIVATE_KEY pays gas in Safe mode.
contract ExecuteGovernance is Governance {
    function run() external {
        _executeGovernance(
            vm.envOr("OWNER_KEY", uint256(0)), vm.envAddress("GOVERNANCE_TARGET"), vm.envBytes("GOVERNANCE_CALLDATA")
        );
    }
}

/// @notice Read-only check of the configured Safe and council binding.
contract CheckGovernance is Governance {
    function run() external view {
        require(_safeMode(), "Governance: expected Safe mode");
        Safe account = _governanceSafe();
        address verifier = vm.envAddress("SECURITY_COUNCIL_VERIFIER");
        (bool success, bytes memory result) = verifier.staticcall(abi.encodeWithSignature("council()"));
        require(
            success && result.length == 32 && abi.decode(result, (address)) == address(account),
            "Governance: council mismatch"
        );
        bytes32 rootId = keccak256("devnet council configuration check");
        (success, result) = verifier.staticcall(abi.encodeWithSignature("attestationDigest(bytes32)", rootId));
        require(success && result.length == 32, "Governance: council digest unavailable");
        (success, result) = address(account)
            .staticcall(abi.encodeWithSignature("getMessageHash(bytes)", abi.encode(abi.decode(result, (bytes32)))));
        require(success && result.length == 32, "Governance: compatibility handler required");
    }
}
