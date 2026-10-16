use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncError {
    BlockPageHasNoCommonAncestor,
    /// A fork needs bounded ancestor state replayed from the chain store, which recently happened;
    /// the page is dropped without penalty and the fork is reconsidered on a later page.
    ForkValidationThrottled,
    /// This node could not replay its own ancestor state to validate a fork, for local reasons such
    /// as a chain store that has not caught up after a reorg. Not the sending peer's fault.
    ForkHistoryUnavailable,
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockPageHasNoCommonAncestor => {
                formatter.write_str("block page has no common ancestor with local chain")
            }
            Self::ForkValidationThrottled => {
                formatter.write_str("fork validation from pruned history is temporarily throttled")
            }
            Self::ForkHistoryUnavailable => {
                formatter.write_str("local chain history is unavailable for fork validation")
            }
        }
    }
}

impl Error for SyncError {}
