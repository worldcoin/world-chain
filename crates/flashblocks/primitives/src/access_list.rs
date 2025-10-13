use alloy_eip7928::AccountChanges;
use alloy_rlp::{RlpDecodable, RlpEncodable};
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockAccessList {
    pub changes: Vec<AccountChanges>,
}
