use std::{error::Error, fmt};

/// Validation failures whose identity drives behavior outside the domain layer.
///
/// Keep ordinary validation messages as `anyhow` errors until a caller needs to
/// branch on them; variants here are a control-flow contract, not an exhaustive
/// catalog of every validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidationError {
    BlockConflictsWithLocalChain { height: u64 },
    UnexpectedBlockHeight { expected: u64, actual: u64 },
    BlockDoesNotExtendLocalTip,
    BlockBeforeFinalizerRankSlot { rank: u32, min_timestamp: u64 },
    BlockTimestampTooFarInFuture,
    BurnBundleParentMismatch,
    BurnAnchorOutsidePendingWindow,
    MineAnchorNotOnChain,
    MineAnchorLimitReached,
}

impl ValidationError {
    pub(crate) fn requests_fork_recovery(self) -> bool {
        matches!(
            self,
            Self::BlockConflictsWithLocalChain { .. }
                | Self::UnexpectedBlockHeight { .. }
                | Self::BlockDoesNotExtendLocalTip
        )
    }

    pub(crate) fn is_fork_relative(self) -> bool {
        matches!(
            self,
            Self::BlockConflictsWithLocalChain { .. }
                | Self::UnexpectedBlockHeight { .. }
                | Self::BlockDoesNotExtendLocalTip
                | Self::BurnBundleParentMismatch
                | Self::BurnAnchorOutsidePendingWindow
                | Self::MineAnchorNotOnChain
        )
    }

    pub(crate) fn is_temporal(self) -> bool {
        matches!(
            self,
            Self::BlockBeforeFinalizerRankSlot { .. } | Self::BlockTimestampTooFarInFuture
        )
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockConflictsWithLocalChain { height } => {
                write!(
                    formatter,
                    "block at height {height} conflicts with local chain"
                )
            }
            Self::UnexpectedBlockHeight { expected, actual } => {
                write!(formatter, "expected block height {expected}, got {actual}")
            }
            Self::BlockDoesNotExtendLocalTip => {
                formatter.write_str("block does not extend local tip")
            }
            Self::BlockBeforeFinalizerRankSlot {
                rank,
                min_timestamp,
            } => write!(
                formatter,
                "block timestamp is before finalizer rank {rank} time slot {min_timestamp}"
            ),
            Self::BlockTimestampTooFarInFuture => {
                formatter.write_str("block timestamp is too far in the future")
            }
            Self::BurnBundleParentMismatch => {
                formatter.write_str("burn bundle parent hash is invalid")
            }
            Self::BurnAnchorOutsidePendingWindow => formatter.write_str(
                "burn transaction anchor is not valid for either of the next two block heights",
            ),
            Self::MineAnchorNotOnChain => {
                formatter.write_str("mine transaction anchor is not on this chain")
            }
            Self::MineAnchorLimitReached => {
                formatter.write_str("mine transaction anchor limit reached")
            }
        }
    }
}

impl Error for ValidationError {}

pub(crate) fn error_has_validation(
    error: &anyhow::Error,
    predicate: impl Fn(ValidationError) -> bool,
) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ValidationError>()
            .copied()
            .is_some_and(&predicate)
    })
}
