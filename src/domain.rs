use std::collections::BTreeSet;

mod address;
#[cfg(test)]
mod adversarial_tests;
mod block;
#[cfg(test)]
mod consolidation_tests;
mod error;
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
mod ledger_v2;
mod mine_policy;
mod mining;
mod profile;
mod protocol;
mod reveal;
mod selection;
mod signature;
mod stratum;
mod ticket;
mod transaction;
mod transaction_v2;
mod validation;
mod vdf;
mod wallet;
pub use address::{
    AddressNetwork, AddressVersion, VersionedAddress, decode_address, decode_versioned_address,
    encode_address, encode_versioned_address, migrate_legacy_address,
};
use block::LeaderProofPayload;
pub use block::{
    Block, BurnLeaderRank, ChainSnapshot, ChainStatus, FinalizerMode, LeaderProof, PreparedBlock,
};
pub(crate) use error::{ValidationError, error_has_validation};
use fork::{FinalityCheckpoint, LeaderScore};
pub(crate) use genesis::genesis_allocation_outpoint;
pub use hex::hex_hash;
pub(crate) use hex::{decode_hex, decode_hex_array, hex_encode};
use ledger_lineage::{
    LineageOwnerValues, UtxoLineageRoot, attach_existing_output_lineage,
    insert_output_with_lineage, newest_lineage_root, output_lineage_root_for_transaction,
    remove_spent_output_lineage, spend_inputs_with_lineage,
};
pub use ledger_ops::reward_outputs_for_block;
use ledger_ops::{
    apply_transaction, credit_reward_output, ensure_block_has_burn, ensure_block_has_burn_from,
    recovery_vdf_seed_for_child, validate_genesis_burn_transaction, vdf_content_commitment,
    vdf_seed_for_child,
};
pub use ledger_state::Ledger;
use ledger_state::unix_now_ms;
pub(crate) use mine_policy::{MINE_RETARGET_WINDOW_BLOCKS, retarget_mine_difficulty_bits};
use mining::{mine_payload, mine_signature};
pub use profile::{GenesisBurn, LaunchProfile};
pub use protocol::{
    Amount, BLOCK_REWARD, BURN_COMMITTEE_SIZE, BURN_LINEAGE_MATURITY_HEIGHTS, DEFAULT_FEE_PER_BYTE,
    DEFAULT_MINE_FEE, DEFAULT_TRANSACTION_FEE, GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
    HYBRID_REWARD_ACTIVATION_HEIGHT, MAX_BLOCK_BYTES, MAX_BURN_BUNDLE_BYTES,
    MAX_PENDING_TRANSACTIONS, MAX_VDF_ROUNDS, MICRO_IUNA, MINE_ACTIONS_PER_ANCHOR_LIMIT,
    MINE_DIFFICULTY_BITS, MINE_FINALIZER_FEE, MINE_REWARD, OBJECTIVE_FINALITY_ACTIVATION_HEIGHT,
    RECOVERY_BLOCK_DELAY_MS, TIP_BOUND_BURN_ACTIVATION_HEIGHT,
    TRANSACTION_REPLAY_PROTECTION_ACTIVATION_HEIGHT, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT,
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
    MaskedBurnV2, default_burn_bundle_hash,
};
use selection::BlockSelection;
pub use signature::{ProtocolPublicKey, ProtocolSignature, SignatureScheme};
pub(crate) use signature::{
    ed25519_public_key, ml_dsa44_public_key, sign_ed25519, sign_ml_dsa44,
    validate_ed25519_public_key, verify_ed25519, verify_ml_dsa44,
};

#[cfg(feature = "fuzzing")]
pub fn fuzz_verify_ml_dsa44(public_key: &[u8], payload: &[u8], signature: &[u8]) {
    let Ok(public_key) = <&[u8; 1_312]>::try_from(public_key) else {
        return;
    };
    let Ok(signature) = <&[u8; 2_420]>::try_from(signature) else {
        return;
    };
    let _ = verify_ml_dsa44(public_key, payload, signature, "fuzz input");
}
pub use stratum::{
    STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, StratumMineShare, StratumMineTemplate,
    pack_stratum_nonce,
};
use stratum::{hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature};
use ticket::{BurnTicket, ticket_block_min_timestamp};
pub use transaction::{
    MineSearchOutcome, OutPoint, TRANSACTION_SIGNING_FORMAT_VERSION, Transaction, TxInput, TxOutput,
};
use transaction::{TransactionSigningDomain, mine_signing_bytes};
pub use transaction_v2::{
    LegacyTransactionId, TRANSACTION_V2_ACTIVATION_HEIGHT, TRANSACTION_V2_WIRE_VERSION,
    TransactionV2, TransactionV2Domain, TransactionV2Input, TransactionV2LegacyInput,
    TransactionV2Output, V2SpendingAuthorization, ensure_transaction_v2_active,
    hybrid_key_commitment_address, transaction_v2_is_active,
};
pub(crate) use validation::minimum_transfer_economic_size_bytes;
pub use validation::validate_address;
use validation::{
    canonical_transaction_size_bytes, validate_hash, validate_protocol_id, validate_signature,
};
#[cfg(feature = "e2e")]
pub use vdf::configure_e2e_vdf_round_divisor_for_tests;
pub use vdf::{
    VdfProgress, VdfProgressPhase, run_vdf, run_vdf_cancellable_with_progress,
    run_vdf_with_progress, verify_vdf,
};
pub use wallet::Wallet;

pub fn burn_committee_slot_count(eligible_rank_count: usize) -> usize {
    eligible_rank_count.min(BURN_COMMITTEE_SIZE)
}

pub fn burn_committee_slot_count_for_owners<'a>(
    owners: impl IntoIterator<Item = &'a str>,
) -> usize {
    burn_committee_slot_count(owners.into_iter().collect::<BTreeSet<_>>().len())
}
