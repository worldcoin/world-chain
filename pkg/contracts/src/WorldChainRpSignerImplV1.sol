// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC165Upgradeable} from "@openzeppelin/contracts-upgradeable/utils/introspection/ERC165Upgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

import {Base} from "./abstract/Base.sol";
import {IActionVerifier} from "./interfaces/IActionVerifier.sol";
import {IRpSigner} from "./interfaces/IRpSigner.sol";

/// @title World Chain RP Signer Implementation V1
/// @author Worldcoin
/// @notice On-chain WIP-101 Relying Party signer for `WORLD_CHAIN_RP_ID`. Stateless in
///         verification: OPRF nodes `eth_call` `verifyRpRequest` before contributing a
///         share, and acceptance returns the magic value `0x35dbc8de`. Uniqueness requests
///         are routed to an owner-managed list of `IActionVerifier`s (e.g. WIP-1002 subsidy
///         accounting, future rate-limit consumers); any one accepting authorizes the
///         request. Session requests are accepted on class prefix alone.
/// @dev RP request envelope codes (`RpInvalidRequest(uint256 code)`):
///        100 BAD_VERSION
///        101 EXPIRED
///        102 NOT_YET_VALID
///        103 TTL_TOO_LONG
///        104 BAD_TIMESTAMP_ORDER
///        105 NON_EMPTY_DATA
///        201 BAD_UNIQUENESS_ACTION
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainRpSignerImplV1 is Base, ERC165Upgradeable, IRpSigner {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                CONSTANTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice WIP-101 magic value returned on acceptance.
    ///         `bytes4(keccak256("verifyRpRequest(uint8,uint256,uint64,uint64,uint256,bytes)"))`.
    bytes4 internal constant MAGICVALUE = 0x35dbc8de;

    /// @notice Wire-format version accepted on the request envelope.
    uint8 internal constant EXPECTED_VERSION = 1;

    /// @notice Upper bound on `expiresAt - createdAt`. See workspace decision note
    ///         "WorldChainRpSigner request TTL bound" for rationale.
    uint64 public constant MAX_REQUEST_TTL = 1 hours;

    /// @notice Session-Proof class prefix. Session Proof `action` values are per-proof random
    ///         OPRF outputs and are NOT signed by the RP — once the prefix is confirmed the
    ///         signer accepts unconditionally. All other actions fall through to verifier
    ///         iteration, which is equality-based and strictly stronger than any class-byte
    ///         check would be (Uniqueness actions are `keccak >> 8` derivations with an
    ///         effectively random top byte, so a strict `class == 0x00` gate would
    ///         spuriously reject legitimate requests).
    uint8 internal constant CLASS_SESSION = 0x02;

    uint256 internal constant CODE_BAD_VERSION = 100;
    uint256 internal constant CODE_EXPIRED = 101;
    uint256 internal constant CODE_NOT_YET_VALID = 102;
    uint256 internal constant CODE_TTL_TOO_LONG = 103;
    uint256 internal constant CODE_BAD_TIMESTAMP_ORDER = 104;
    uint256 internal constant CODE_NON_EMPTY_DATA = 105;
    uint256 internal constant CODE_BAD_UNIQUENESS_ACTION = 201;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 EVENTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once when the implementation is initialized behind its proxy.
    event RpSignerImplInitialized(address indexed owner);

    /// @notice Emitted when an action verifier is registered.
    event ActionVerifierAdded(IActionVerifier indexed verifier);

    /// @notice Emitted when an action verifier is deregistered.
    event ActionVerifierRemoved(IActionVerifier indexed verifier);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 ERRORS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when a required address parameter is the zero address.
    error AddressZero();

    /// @notice Thrown by `removeVerifier` when `verifier` is not registered.
    error VerifierNotFound(IActionVerifier verifier);

    /// @notice Thrown by `addVerifier` (and `initialize`) when `verifier` is already in the
    ///         registered set.
    error VerifierAlreadyRegistered(IActionVerifier verifier);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 STORAGE                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Owner-managed list of action verifiers. A Uniqueness `action` is authorized
    ///         iff at least one registered verifier reports `isValidAction(action) == true`.
    IActionVerifier[] internal _verifiers;

    /// @dev Storage gap for future upgrades. See Base.sol for the inheritance-chain idiom.
    uint256[49] private __gap;

    ///////////////////////////////////////////////////////////////////////////////
    ///                            INIT / UPGRADE                               ///
    ///////////////////////////////////////////////////////////////////////////////

    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the implementation behind its proxy.
    /// @param initialVerifiers Initial action-verifier set. MAY be empty (Session-only mode
    ///         until `addVerifier` is called).
    /// @param owner_ Owner of the proxy. Authorized for `addVerifier`, `removeVerifier`, and
    ///         UUPS upgrades.
    function initialize(IActionVerifier[] calldata initialVerifiers, address owner_)
        external
        virtual
        reinitializer(1)
    {
        if (owner_ == address(0)) revert AddressZero();
        __Base_init(owner_);
        __ERC165_init();
        uint256 n = initialVerifiers.length;
        for (uint256 i = 0; i < n; ++i) {
            _addVerifier(initialVerifiers[i]);
        }
        emit RpSignerImplInitialized(owner_);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IRpSigner
    /// @dev Envelope checks → Session fast-route → verifier iteration. Anything that isn't
    ///      class `0x02` (Session) falls through to verifier iteration, where each verifier
    ///      decides via equality against its own action derivation. Verifier equality is
    ///      strictly stronger than any class-prefix check would be, so the class byte is
    ///      used only to short-circuit unconditional Session acceptance.
    function verifyRpRequest(
        uint8 version,
        uint256, /*nonce*/
        uint64 createdAt,
        uint64 expiresAt,
        uint256 action,
        bytes calldata data
    ) external view virtual onlyProxy returns (bytes4) {
        if (version != EXPECTED_VERSION) revert RpInvalidRequest(CODE_BAD_VERSION);
        if (data.length != 0) revert RpInvalidRequest(CODE_NON_EMPTY_DATA);
        if (createdAt > expiresAt) revert RpInvalidRequest(CODE_BAD_TIMESTAMP_ORDER);
        if (createdAt > block.timestamp) revert RpInvalidRequest(CODE_NOT_YET_VALID);
        if (expiresAt < block.timestamp) revert RpInvalidRequest(CODE_EXPIRED);
        if (expiresAt - createdAt > MAX_REQUEST_TTL) revert RpInvalidRequest(CODE_TTL_TOO_LONG);

        if (uint8(action >> 240) == CLASS_SESSION) return MAGICVALUE;

        uint256 n = _verifiers.length;
        for (uint256 i = 0; i < n; ++i) {
            if (_verifiers[i].isValidAction(action)) return MAGICVALUE;
        }
        revert RpInvalidRequest(CODE_BAD_UNIQUENESS_ACTION);
    }

    /// @notice The currently registered action verifiers, in insertion order modulo
    ///         swap-and-pop removals.
    function getVerifiers() external view virtual onlyProxy returns (IActionVerifier[] memory) {
        return _verifiers;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Register `verifier`. Owner-only. Reverts on zero address or duplicate.
    function addVerifier(IActionVerifier verifier) external virtual onlyProxy onlyOwner {
        _addVerifier(verifier);
    }

    /// @notice Deregister `verifier`. Owner-only. Swap-and-pop; preserves no ordering.
    function removeVerifier(IActionVerifier verifier) external virtual onlyProxy onlyOwner {
        uint256 n = _verifiers.length;
        for (uint256 i = 0; i < n; ++i) {
            if (_verifiers[i] == verifier) {
                _verifiers[i] = _verifiers[n - 1];
                _verifiers.pop();
                emit ActionVerifierRemoved(verifier);
                return;
            }
        }
        revert VerifierNotFound(verifier);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                ERC-165                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ERC165Upgradeable
    function supportsInterface(bytes4 interfaceId)
        public
        view
        virtual
        override(ERC165Upgradeable, IERC165)
        returns (bool)
    {
        return interfaceId == type(IRpSigner).interfaceId || super.supportsInterface(interfaceId);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    function _addVerifier(IActionVerifier verifier) internal {
        if (address(verifier) == address(0)) revert AddressZero();
        uint256 n = _verifiers.length;
        for (uint256 i = 0; i < n; ++i) {
            if (_verifiers[i] == verifier) revert VerifierAlreadyRegistered(verifier);
        }
        _verifiers.push(verifier);
        emit ActionVerifierAdded(verifier);
    }
}
