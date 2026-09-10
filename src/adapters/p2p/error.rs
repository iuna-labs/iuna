use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncError {
    BlockPageHasNoCommonAncestor,
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockPageHasNoCommonAncestor => {
                formatter.write_str("block page has no common ancestor with local chain")
            }
        }
    }
}

impl Error for SyncError {}
