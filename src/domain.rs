use std::collections::BTreeSet;

#[cfg(test)]
mod adversarial_tests;
mod block;
mod fork;
mod genesis;
mod hex;
mod ledger_apply;
mod ledger_builders;
mod ledger_chain;
mod ledger_consensus;
mod ledger_lineage;
mod ledger_mempool;
mod ledger_ops;
mod ledger_pending;
mod ledger_prepare;
mod ledger_queries;
mod ledger_reveal;
mod ledger_state;
mod mine_policy;
mod mining;
mod profile;
mod protocol;
mod reveal;
mod selection;
mod stratum;
mod ticket;
mod transaction;
mod validation;
mod vdf;
mod wallet;
use block::LeaderProofPayload;
pub use block::{
    Block, BurnLeaderRank, ChainSnapshot, ChainStatus, FinalizerMode, LeaderProof, PreparedBlock,
};
use fork::LeaderScore;
use genesis::genesis_allocation_outpoint;
pub use hex::hex_hash;
use hex::{decode_hex, decode_hex_array, hex_encode};
use ledger_lineage::{
    LineageOwnerValues, UtxoLineageRoot, insert_output_with_lineage,
    output_lineage_root_for_transaction, spend_inputs_with_lineage,
};
use ledger_ops::{
    apply_transaction, credit_reward_output, ensure_block_has_burn, ensure_block_has_burn_from,
    recovery_vdf_seed_for_child, validate_genesis_burn_transaction, vdf_seed_for_child,
};
pub use ledger_state::Ledger;
use ledger_state::unix_now_ms;
use mining::{mine_payload, mine_signature};
pub use profile::{GenesisBurn, LaunchProfile};
pub use protocol::{
    Amount, BLOCK_REWARD, BURN_COMMITTEE_SIZE, BURN_LINEAGE_MATURITY_HEIGHTS, DEFAULT_FEE_PER_BYTE,
    DEFAULT_MINE_FEE, DEFAULT_TRANSACTION_FEE, MAX_BLOCK_BYTES, MAX_BURN_BUNDLE_BYTES,
    MAX_PENDING_TRANSACTIONS, MAX_VDF_ROUNDS, MICRO_IUNA, MINE_ACTIONS_PER_ANCHOR_LIMIT,
    MINE_DIFFICULTY_BITS, MINE_FINALIZER_FEE, MINE_REWARD, RECOVERY_BLOCK_DELAY_MS,
    TransactionSubmitOutcome, VDF_TARGET_BLOCK_MS,
};
use protocol::{
    BLOCK_MEDIAN_TIME_PAST_WINDOW, DEFAULT_TICKET_EXPIRY_WINDOW, DEFAULT_TICKET_MATURITY_DELAY,
    FORK_FINALITY_DEPTH, HASH_BYTES, MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS, MAX_BLOCK_TRANSACTIONS,
    MAX_ORPHAN_TRANSACTIONS, MAX_PENDING_POOL_BYTES, PUBLIC_KEY_BYTES, SIGNATURE_BYTES,
};
use reveal::BurnBundlePayload;
pub use reveal::{
    BurnBundle, BurnBundleSection, BurnBundleSignature, BurnCommitteeMember, MaskedBurn,
    default_burn_bundle_hash,
};
use selection::BlockSelection;
pub use stratum::{
    STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, StratumMineShare, StratumMineTemplate,
    pack_stratum_nonce,
};
use stratum::{hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature};
use ticket::{BurnTicket, ticket_block_min_timestamp};
pub use transaction::{MineSearchOutcome, OutPoint, Transaction, TxInput, TxOutput};
pub use validation::validate_address;
use validation::{
    canonical_transaction_size_bytes, validate_hash, validate_protocol_id, validate_signature,
};
pub use vdf::{VdfProgress, VdfProgressPhase, run_vdf, run_vdf_with_progress, verify_vdf};
pub use wallet::Wallet;

pub fn burn_committee_slot_count(eligible_rank_count: usize) -> usize {
    eligible_rank_count.min(BURN_COMMITTEE_SIZE)
}

pub fn burn_committee_slot_count_for_owners<'a>(
    owners: impl IntoIterator<Item = &'a str>,
) -> usize {
    burn_committee_slot_count(owners.into_iter().collect::<BTreeSet<_>>().len())
}
