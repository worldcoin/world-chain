// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";

import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";

library ProofSystemDeploymentLib {
    function currentBondVault(IDisputeGameFactory factory) internal view returns (IWLDStakingVault bondVault) {
        IDisputeGame currentImplementation = factory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE);
        if (address(currentImplementation) == address(0)) return IWLDStakingVault(address(0));

        try IMultiProofGame(address(currentImplementation)).bondVault() returns (IWLDStakingVault currentBondVault_) {
            bondVault = currentBondVault_;
        } catch {
            // The first WLD migration may replace a legacy implementation without this getter.
        }
    }
}
