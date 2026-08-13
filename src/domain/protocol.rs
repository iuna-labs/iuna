pub type Amount = u64;

pub const MICRO_IUNA: Amount = 1_000_000;
pub const BLOCK_REWARD: Amount = MICRO_IUNA;
pub const MINE_REWARD: Amount = MICRO_IUNA;
pub const MINE_FINALIZER_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_MINE_FEE: Amount = MINE_FINALIZER_FEE;
pub const DEFAULT_TRANSACTION_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_FEE_PER_BYTE: Amount = 1;
pub const MAX_BLOCK_BYTES: usize = 100_000;
pub const VDF_TARGET_BLOCK_MS: u64 = 5 * 60 * 1_000;
pub const RECOVERY_BLOCK_DELAY_MS: u64 = VDF_TARGET_BLOCK_MS * 6;
pub const MAX_VDF_ROUNDS: u64 = i64::MAX as u64;
pub const MINE_DIFFICULTY_BITS: u32 = 12;
pub const MINE_ACTIONS_PER_ANCHOR_LIMIT: usize = 2;
pub const MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS: u64 = 20;
pub const REVEAL_COMMITTEE_SIZE: usize = 3;
pub const UNIQUE_OWNER_REVEAL_COMMITTEE_HEIGHT: u64 = 500;
pub const MAX_REVEAL_BUNDLE_BYTES: usize = 10_000;
pub const BLINDED_FEE_BPS_DENOMINATOR: u64 = 10_000;
pub const BLINDED_COMMITTER_FEE_BPS: u64 = 3_500;
pub const BLINDED_REVEAL_FINALIZER_FEE_BPS: u64 = 3_500;
pub const BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS: u64 = 1_000;

pub const MAX_PENDING_TRANSACTIONS: usize = 10_000;
pub(super) const MAX_ORPHAN_TRANSACTIONS: usize = 1_024;
pub(super) const MAX_BLOCK_TRANSACTIONS: usize = 1_000;
pub(super) const DEFAULT_TICKET_MATURITY_DELAY: u64 = 3;
pub(super) const DEFAULT_TICKET_EXPIRY_WINDOW: u64 = 3;
pub(super) const MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS: u64 = 2 * 60 * 1_000;
pub(super) const BLOCK_MEDIAN_TIME_PAST_WINDOW: usize = 11;
pub(super) const FORK_FINALITY_DEPTH: u64 = 6;
pub(super) const PUBLIC_KEY_BYTES: usize = 32;
pub(super) const HASH_BYTES: usize = 32;
pub(super) const SIGNATURE_BYTES: usize = 64;
pub(super) const BLINDED_KEY_BYTES: usize = 32;
pub(super) const BLINDED_NONCE_BYTES: usize = 12;

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
