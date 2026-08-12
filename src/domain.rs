#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};

mod blinded;
mod block;
mod fork;
mod genesis;
mod hex;
mod history;
mod ledger_apply;
mod ledger_builders;
mod ledger_chain;
mod ledger_consensus;
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
use blinded::{ActiveBlindedTransaction, blinded_fee_share};
#[cfg(test)]
use blinded::{
    blinded_committer_fee_outpoint, blinded_executor_fee_outpoint, blinded_expiry_change_outpoint,
    blinded_reveal_bundle_signer_fee_outpoint, blinded_transaction_commitment,
    credit_blinded_fee_outputs, decrypt_blinded_payload, decrypt_blinded_transaction,
};
use block::LeaderProofPayload;
pub use block::{
    Block, BurnLeaderRank, ChainSnapshot, ChainStatus, FinalizerMode, LeaderProof, PreparedBlock,
};
use fork::LeaderScore;
#[cfg(test)]
use genesis::balances_from_utxos;
use genesis::genesis_allocation_outpoint;
pub use hex::hex_hash;
use hex::{decode_hex, decode_hex_array, hex_encode};
pub use history::revealed_blinded_transactions;
#[cfg(test)]
use ledger_ops::estimated_block_selection_size_bytes;
#[cfg(test)]
use ledger_ops::fee_reward;
#[cfg(test)]
use ledger_ops::reward_outpoint;
use ledger_ops::{
    apply_transaction, credit_reward_output, ensure_block_has_burn, ensure_block_has_burn_from,
    ensure_outputs_do_not_overflow, ensure_single_input_owner_for_inputs,
    recovery_vdf_seed_for_child, validate_genesis_burn_transaction, vdf_seed_for_child,
};
pub use ledger_state::Ledger;
use ledger_state::unix_now_ms;
#[cfg(test)]
use mine_policy::MINE_RETARGET_WINDOW_BLOCKS;
#[cfg(test)]
use mine_policy::{
    MINE_MAX_ANCHOR_AGE_BLOCKS, MINE_MAX_RETARGET_STEP_BITS, MINE_MIN_DIFFICULTY_BITS,
};
use mining::{mine_payload, mine_signature};
pub use profile::{GenesisBurn, LaunchProfile};
pub use protocol::{
    Amount, BLINDED_COMMITTER_FEE_BPS, BLINDED_FEE_BPS_DENOMINATOR,
    BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS, BLINDED_REVEAL_FINALIZER_FEE_BPS, BLOCK_REWARD,
    DEFAULT_FEE_PER_BYTE, DEFAULT_MINE_FEE, DEFAULT_TRANSACTION_FEE,
    MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MAX_BLOCK_BYTES, MAX_PENDING_TRANSACTIONS,
    MAX_REVEAL_BUNDLE_BYTES, MAX_VDF_ROUNDS, MICRO_IUNA, MINE_ACTIONS_PER_ANCHOR_LIMIT,
    MINE_DIFFICULTY_BITS, MINE_FINALIZER_FEE, MINE_REWARD, RECOVERY_BLOCK_DELAY_MS,
    REVEAL_COMMITTEE_SIZE, TransactionSubmitOutcome, VDF_TARGET_BLOCK_MS,
};
use protocol::{
    BLINDED_KEY_BYTES, BLINDED_NONCE_BYTES, BLOCK_MEDIAN_TIME_PAST_WINDOW,
    DEFAULT_TICKET_EXPIRY_WINDOW, DEFAULT_TICKET_MATURITY_DELAY, FORK_FINALITY_DEPTH, HASH_BYTES,
    MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS, MAX_BLOCK_TRANSACTIONS, MAX_ORPHAN_TRANSACTIONS,
    PUBLIC_KEY_BYTES, SIGNATURE_BYTES,
};
use reveal::RevealBundlePayload;
#[cfg(test)]
use reveal::reveal_bundle_hashes;
pub use reveal::{
    MaskedBlindedReveal, RevealBundle, RevealBundleSection, RevealBundleSignature,
    RevealCommitteeMember, default_reveal_bundle_hash,
};
use selection::BlockSelection;
pub use stratum::{
    STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, StratumMineShare, StratumMineTemplate,
    pack_stratum_nonce,
};
use stratum::{hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature};
use ticket::{BurnTicket, ticket_block_min_timestamp};
#[cfg(test)]
use ticket::{
    MISSED_FALLBACK_TICKET_INVALIDATION_HEIGHT, consume_leader_ticket, ranked_tickets_for_height,
    ticket_is_eligible_for_height,
};
pub use transaction::{
    BlindedReveal, BlindedTransaction, BuiltBlindedTransaction, MineSearchOutcome, OutPoint,
    OwnedBlindedTransaction, RevealedBlindedTransaction, Transaction, TxInput, TxOutput,
};
#[cfg(test)]
use transaction::{
    UnsignedTxInput, UnsignedUtxoTransaction, signed_blinded_inputs, unsigned_inputs,
};
pub use validation::validate_address;
use validation::{
    canonical_transaction_size_bytes, validate_hash, validate_protocol_id, validate_signature,
};
#[cfg(test)]
use vdf::{
    MAX_VDF_RETARGET_OBSERVED_BLOCK_MS, MIN_VDF_RETARGET_OBSERVED_BLOCK_MS,
    VDF_RETARGET_DEADBAND_PERCENT, clamped_vdf_retarget_observed_block_ms, retarget_vdf_rounds,
    vdf_retarget_observed_block_ms,
};
pub use vdf::{run_vdf, verify_vdf};
pub use wallet::Wallet;

pub fn reveal_committee_slot_count(eligible_rank_count: usize) -> usize {
    eligible_rank_count.min(REVEAL_COMMITTEE_SIZE)
}

pub fn blinded_reveal_finalizer_fee(
    fee: Amount,
    included_bundle_count: usize,
    available_bundle_slots: usize,
) -> Amount {
    if included_bundle_count == 0 {
        return 0;
    }
    let available_bundle_slots = available_bundle_slots.clamp(1, REVEAL_COMMITTEE_SIZE);
    let included_bundle_count = included_bundle_count.min(available_bundle_slots);
    let full_share = blinded_fee_share(fee, BLINDED_REVEAL_FINALIZER_FEE_BPS);
    ((full_share as u128 * included_bundle_count as u128) / available_bundle_slots as u128)
        as Amount
}

#[cfg(test)]
mod tests;
