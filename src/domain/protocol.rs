pub type Amount = u64;

pub const MICRO_IUNA: Amount = 1_000_000;
pub const BLOCK_REWARD: Amount = MICRO_IUNA;
pub const MINE_REWARD: Amount = MICRO_IUNA;
pub const MINE_FINALIZER_FEE: Amount = MICRO_IUNA;
/// Consensus height at which every newly-created reward output must use an
/// address-v1 hybrid Ed25519 + ML-DSA payout address.
pub const HYBRID_REWARD_ACTIVATION_HEIGHT: u64 = 3_750;
/// Consensus height at which authenticated address-v1 owners can represent the
/// same legacy Ed25519 identity in burn-committee lineage selection.
pub const HYBRID_LINEAGE_IDENTITY_ACTIVATION_HEIGHT: u64 = 4_750;
pub const DEFAULT_MINE_FEE: Amount = MINE_FINALIZER_FEE;
pub const DEFAULT_TRANSACTION_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_FEE_PER_BYTE: Amount = 1;
pub const MAX_BLOCK_BYTES: usize = 1_000_000;
/// Number of blocks in an independently persisted and synchronized chain segment.
pub const CHAIN_SEGMENT_BLOCKS: usize = 256;
#[cfg(not(feature = "e2e"))]
pub const VDF_TARGET_BLOCK_MS: u64 = 10 * 60 * 1_000;
#[cfg(feature = "e2e")]
pub const VDF_TARGET_BLOCK_MS: u64 = 5_000;
pub const RECOVERY_BLOCK_DELAY_MS: u64 = VDF_TARGET_BLOCK_MS * 6;
pub const MAX_VDF_ROUNDS: u64 = i64::MAX as u64;
pub const MINE_DIFFICULTY_BITS: u32 = 12;
pub const MINE_ACTIONS_PER_ANCHOR_LIMIT: usize = 2;
pub const BURN_COMMITTEE_SIZE: usize = 5;
pub const MAX_BURN_BUNDLE_BYTES: usize = 10_000;
pub const BURN_LINEAGE_MATURITY_HEIGHTS: u64 = 20;
pub const GRINDING_RESISTANCE_ACTIVATION_HEIGHT: u64 = 1_000;
pub const TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT: u64 = 1_000;
pub const TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT: u64 = 1_000;
pub const OBJECTIVE_FINALITY_ACTIVATION_HEIGHT: u64 = 1_000;
pub const TIP_BOUND_BURN_ACTIVATION_HEIGHT: u64 = 1_000;

pub const MAX_PENDING_TRANSACTIONS: usize = 10_000;
pub(super) const MAX_PENDING_POOL_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_ORPHAN_TRANSACTIONS: usize = 1_024;
pub(super) const MAX_BLOCK_TRANSACTIONS: usize = 1_000;
pub(super) const DEFAULT_TICKET_MATURITY_DELAY: u64 = 3;
pub(super) const DEFAULT_TICKET_EXPIRY_WINDOW: u64 = 3;
pub(super) const MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS: u64 = 2 * 60 * 1_000;
pub(super) const BLOCK_MEDIAN_TIME_PAST_WINDOW: usize = 11;
pub(super) const FORK_FINALITY_DEPTH: u64 = 6;
pub(super) const PUBLIC_KEY_BYTES: usize = super::SignatureScheme::Ed25519.public_key_bytes();
pub(super) const HASH_BYTES: usize = 32;
pub(super) const SIGNATURE_BYTES: usize = super::SignatureScheme::Ed25519.signature_bytes();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionSubmitOutcome {
    Added,
    AlreadyKnown,
    ConflictsWithPending,
}

impl TransactionSubmitOutcome {
    pub fn added(self) -> bool {
        matches!(self, Self::Added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_candidate_protocol_parameters_are_frozen() {
        assert_eq!(MICRO_IUNA, 1_000_000);
        assert_eq!(BLOCK_REWARD, MICRO_IUNA);
        assert_eq!(MINE_REWARD, MICRO_IUNA);
        assert_eq!(MINE_FINALIZER_FEE, MICRO_IUNA);
        assert_eq!(HYBRID_REWARD_ACTIVATION_HEIGHT, 3_750);
        assert_eq!(HYBRID_LINEAGE_IDENTITY_ACTIVATION_HEIGHT, 4_750);
        assert_eq!(MAX_BLOCK_BYTES, 1_000_000);
        #[cfg(not(feature = "e2e"))]
        assert_eq!(VDF_TARGET_BLOCK_MS, 10 * 60 * 1_000);
        #[cfg(feature = "e2e")]
        assert_eq!(VDF_TARGET_BLOCK_MS, 5_000);
        assert_eq!(RECOVERY_BLOCK_DELAY_MS, 6 * VDF_TARGET_BLOCK_MS);
        assert_eq!(MINE_DIFFICULTY_BITS, 12);
        assert_eq!(MINE_ACTIONS_PER_ANCHOR_LIMIT, 2);
        assert_eq!(BURN_COMMITTEE_SIZE, 5);
        assert_eq!(MAX_BURN_BUNDLE_BYTES, 10_000);
        assert_eq!(BURN_LINEAGE_MATURITY_HEIGHTS, 20);
        assert_eq!(GRINDING_RESISTANCE_ACTIVATION_HEIGHT, 1_000);
        assert_eq!(TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT, 1_000);
        assert_eq!(TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT, 1_000);
        assert_eq!(OBJECTIVE_FINALITY_ACTIVATION_HEIGHT, 1_000);
        assert_eq!(TIP_BOUND_BURN_ACTIVATION_HEIGHT, 1_000);
        assert_eq!(MAX_PENDING_TRANSACTIONS, 10_000);
        assert_eq!(MAX_PENDING_POOL_BYTES, 8 * 1024 * 1024);
        assert_eq!(MAX_ORPHAN_TRANSACTIONS, 1_024);
        assert_eq!(MAX_BLOCK_TRANSACTIONS, 1_000);
        assert_eq!(DEFAULT_TICKET_MATURITY_DELAY, 3);
        assert_eq!(DEFAULT_TICKET_EXPIRY_WINDOW, 3);
        assert_eq!(MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS, 2 * 60 * 1_000);
        assert_eq!(BLOCK_MEDIAN_TIME_PAST_WINDOW, 11);
        assert_eq!(FORK_FINALITY_DEPTH, 6);
        assert_eq!(PUBLIC_KEY_BYTES, 32);
        assert_eq!(HASH_BYTES, 32);
        assert_eq!(SIGNATURE_BYTES, 64);
        assert_eq!(
            crate::domain::ticket::MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT,
            300
        );
    }
}
