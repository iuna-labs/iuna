#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ForkPoint {
    pub(super) common_ancestor_height: u64,
}

impl ForkPoint {
    pub(super) fn first_diverging_height(self) -> u64 {
        self.common_ancestor_height + 1
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LeaderScore {
    pub(super) finalizer_mode_rank: u8,
    pub(super) finalizer_rank: u32,
    pub(super) proof_rank: String,
}

impl Ord for LeaderScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.finalizer_mode_rank
            .cmp(&other.finalizer_mode_rank)
            .then_with(|| self.finalizer_rank.cmp(&other.finalizer_rank))
            .then_with(|| self.proof_rank.cmp(&other.proof_rank))
    }
}

impl PartialOrd for LeaderScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForkQuality {
    LocalBetter,
    RemoteBetter,
    Equal,
}

impl From<std::cmp::Ordering> for ForkQuality {
    fn from(ordering: std::cmp::Ordering) -> Self {
        match ordering {
            std::cmp::Ordering::Less => Self::LocalBetter,
            std::cmp::Ordering::Equal => Self::Equal,
            std::cmp::Ordering::Greater => Self::RemoteBetter,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForkChoice {
    KeepLocal,
    SwitchToCandidate,
}

#[cfg(test)]
mod tests {
    use super::{ForkPoint, ForkQuality, LeaderScore};

    #[test]
    fn fork_point_reports_first_diverging_height() {
        assert_eq!(
            ForkPoint {
                common_ancestor_height: 41
            }
            .first_diverging_height(),
            42
        );
    }

    #[test]
    fn leader_score_orders_by_mode_rank_then_finalizer_rank_then_proof_rank() {
        let base = LeaderScore {
            finalizer_mode_rank: 0,
            finalizer_rank: 0,
            proof_rank: "b".to_string(),
        };

        assert!(
            base < LeaderScore {
                finalizer_mode_rank: 1,
                finalizer_rank: 0,
                proof_rank: "a".to_string(),
            }
        );
        assert!(
            base < LeaderScore {
                finalizer_mode_rank: 0,
                finalizer_rank: 1,
                proof_rank: "a".to_string(),
            }
        );
        assert!(
            base > LeaderScore {
                finalizer_mode_rank: 0,
                finalizer_rank: 0,
                proof_rank: "a".to_string(),
            }
        );
    }

    #[test]
    fn fork_quality_maps_ordering_without_inversion() {
        assert_eq!(
            ForkQuality::from(std::cmp::Ordering::Less),
            ForkQuality::LocalBetter
        );
        assert_eq!(
            ForkQuality::from(std::cmp::Ordering::Equal),
            ForkQuality::Equal
        );
        assert_eq!(
            ForkQuality::from(std::cmp::Ordering::Greater),
            ForkQuality::RemoteBetter
        );
    }
}
