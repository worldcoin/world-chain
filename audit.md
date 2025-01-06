# Internal Audit Notes

**Commit Hash**: `<commit-hash>`

---

## Finding: Title

**Type**: Vulnerability/Best Practice  
**Description**:  
`<What's the issue?>`  

**Location**:  
`<File and line numbers>`  

**Recommendation**:  
`<How should this be addressed?>`

## Finding: No docs on PBHEntryPointImplInitialized

**Type**: Best Parctice
**Description**:
Missing docs on an event

**Location**:
contracts/src/PBHEntryPointImplV1.sol:82

## Finding: Wrong reinitializer value?

**Type**: Best Practice
**Description**:
The reinitializer index is set to 1, but natspec comment above suggest it's zero indexed. So should be 0.

**Location**:
contracts/src/PBHEntryPointImplV1.sol:153

## Finding: Misinformation in docs

**Type**: Best Practice
**Description**:
The docs say _worldId can be set to zero address and then verification will be off chain. But code will revert if that's the case.

**Location**:
contracts/src/PBHEntryPointImplV1.sol:146

**Notes**:
Furthermore the `verifyPbh` function checks if worldId is a zero address - disabling proof verification in that case.
Line: contracts/src/PBHEntryPointImplV1.sol:208

## Finding: Confusing inheritance

**Type**: Best Practice
**Description**:
The PBHEntryPointImplV1 contract inherits WorldIDImpl. Despite not actualy providing any world id related functionality.

The inheritance is there seemingly to inherit ownability and upgradeability functionality - which could also be solved by directly inheriting UUPSUpgradeable, etc.  

Additionally WorldIDImpl disallows renouncing ownership. 
## Finding: Undocumented test dependency

**Type**: Best Practice
**Description**:
Contract tests require an RPC url to be provided to forge via the `-f` flag. Without this tests are failing.

