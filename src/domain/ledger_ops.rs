use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::hex::hex_hash;
use super::reveal::{BurnBundleSection, canonical_burn_bundle_hashes};
use super::selection::{TransactionKind, fee_rate_key};
use super::ticket::ticket_is_eligible_for_height;
use super::transaction::Transaction;
use super::vdf::vdf_solution_placeholder;
use super::{
    Amount, BURN_COMMITTEE_SIZE, Block, BlockSelection, BurnCommitteeMember, BurnTicket,
    FinalizerMode, LeaderProof, LeaderProofPayload, MINE_REWARD, OutPoint, PUBLIC_KEY_BYTES,
    RECOVERY_BLOCK_DELAY_MS, SIGNATURE_BYTES, TxInput, TxOutput, decode_hex_array,
    validate_address, validate_hash, validate_protocol_id, validate_signature,
};

pub(super) fn validate_genesis_allocations(
    genesis_allocations: &BTreeMap<String, Amount>,
) -> Result<()> {
    for address in genesis_allocations.keys() {
        validate_address(address, "genesis allocation")?;
    }
    Ok(())
}

pub(super) fn validate_transaction_inputs(inputs: &[TxInput]) -> Result<()> {
    for input in inputs {
        validate_protocol_id(&input.outpoint.txid, "input outpoint txid")?;
        validate_address(&input.owner, "input owner")?;
        validate_signature(&input.signature, "input signature")?;
    }
    Ok(())
}

pub(super) fn validate_transaction_outputs(outputs: &[TxOutput]) -> Result<()> {
    for output in outputs {
        validate_address(&output.address, "output recipient")?;
    }
    Ok(())
}

pub(super) fn validate_genesis_burn_transaction(transaction: &Transaction) -> Result<()> {
    let Transaction::Burn {
        inputs,
        change,
        fee,
        signature,
        ..
    } = transaction
    else {
        bail!("genesis only supports burn transactions");
    };
    if *fee != 0 {
        bail!("genesis burn fee must be zero");
    }
    validate_hash(signature, "genesis burn signature")?;
    validate_transaction_outputs(change)?;
    for input in inputs {
        validate_hash(&input.outpoint.txid, "genesis burn input outpoint txid")?;
        validate_address(&input.owner, "genesis burn input owner")?;
        if input.signature != "genesis" {
            bail!("genesis burn input signature is invalid");
        }
    }
    Ok(())
}

pub(super) fn estimated_block_selection_size_bytes(
    selection: &BlockSelection,
    recovery: bool,
    burn_bundle_section: &BurnBundleSection,
) -> Result<usize> {
    let block = Block {
        height: u64::MAX,
        prev_hash: "f".repeat(64),
        timestamp_ms: u64::MAX,
        miner: "f".repeat(64),
        finalizer_mode: if recovery {
            FinalizerMode::Recovery
        } else {
            FinalizerMode::Ticket
        },
        finalizer_rank: 0,
        reward: u64::MAX,
        vdf_rounds: u64::MAX,
        vdf_output: vdf_solution_placeholder(),
        leader_proof: (!recovery).then(|| LeaderProof {
            ticket_id: "f".repeat(64),
            public_key: "f".repeat(64),
            signature: "f".repeat(128),
        }),
        burn_bundle_section: burn_bundle_section.clone(),
        transactions: selection.transactions.clone(),
        hash: "f".repeat(64),
    };
    block.serialized_size_bytes()
}

pub(super) fn ensure_transaction_fits_empty_block(
    transaction: &Transaction,
    max_block_bytes: usize,
) -> Result<()> {
    let selection = BlockSelection {
        transactions: vec![transaction.clone()],
    };
    if estimated_block_selection_size_bytes(&selection, false, &BurnBundleSection::default())?
        > max_block_bytes
    {
        bail!("transaction exceeds max block size");
    }
    Ok(())
}

pub(super) fn verify_leader_proof(block: &Block, tickets: &[BurnTicket]) -> Result<()> {
    let Some(proof) = &block.leader_proof else {
        bail!("block is missing leader proof");
    };
    if proof.public_key != block.miner {
        bail!("leader proof public key does not match block finalizer");
    }
    let ticket = tickets
        .iter()
        .find(|ticket| {
            ticket.id == proof.ticket_id && ticket_is_eligible_for_height(ticket, block.height)
        })
        .context("leader ticket is not pending for this height")?;
    if ticket.owner != block.miner {
        bail!("leader ticket owner does not match block finalizer");
    }
    if ticket.eligible_from_height > block.height {
        bail!("leader ticket is not mature");
    }

    let payload = LeaderProofPayload {
        height: block.height,
        prev_hash: block.prev_hash.clone(),
        finalizer_rank: block.finalizer_rank,
        vdf_output: block.vdf_output.clone(),
        ticket_id: ticket.id.clone(),
        ticket_amount: ticket.amount,
        ticket_owner: ticket.owner.clone(),
    };
    verify_leader_signature(proof, &payload)?;
    Ok(())
}

pub(super) fn verify_leader_signature(
    proof: &LeaderProof,
    payload: &LeaderProofPayload,
) -> Result<()> {
    verify_address_signature(
        &proof.public_key,
        &payload.canonical(),
        &proof.signature,
        "leader",
    )
}

pub(super) fn verify_address_signature(
    address: &str,
    payload: &str,
    signature: &str,
    label: &str,
) -> Result<()> {
    let public_key = decode_hex_array::<PUBLIC_KEY_BYTES>(address)
        .with_context(|| format!("invalid {label} public key {address}"))?;
    let signature = decode_hex_array::<SIGNATURE_BYTES>(signature)
        .with_context(|| format!("invalid {label} signature hex"))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .with_context(|| format!("invalid {label} public key"))?;
    let signature = Signature::from_bytes(&signature);
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .with_context(|| format!("{label} signature is invalid"))
}

pub(super) fn vdf_seed_for_child(
    prev_hash: &str,
    height: u64,
    bundle_hashes: &[String; super::BURN_COMMITTEE_SIZE],
) -> String {
    hex_hash(format!(
        "iuna-vdf-child:{prev_hash}:{height}:{}",
        canonical_burn_bundle_hashes(bundle_hashes)
    ))
}

pub(super) fn recovery_vdf_seed_for_child(
    prev_hash: &str,
    height: u64,
    timestamp_ms: u64,
    bundle_hashes: &[String; super::BURN_COMMITTEE_SIZE],
) -> String {
    hex_hash(format!(
        "iuna-recovery-vdf-child:{prev_hash}:{height}:{timestamp_ms}:{}",
        canonical_burn_bundle_hashes(bundle_hashes)
    ))
}

pub(super) fn apply_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    transaction.verify_signature()?;
    match transaction {
        Transaction::Mine { recipient, .. } => {
            let output = TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            };
            ensure_outputs_do_not_overflow(utxos, std::slice::from_ref(&output))?;
            utxos.insert(
                OutPoint {
                    txid: transaction.signature().to_string(),
                    index: 0,
                },
                output,
            );
            return Ok(());
        }
        Transaction::Transfer { .. } | Transaction::Burn { .. } => {}
    }
    ensure_single_input_owner(transaction)?;
    let input_total = spend_inputs(transaction, utxos)?;
    let outputs = transaction.outputs();
    let output_total = outputs.iter().try_fold(0_u64, |total, output| {
        total
            .checked_add(output.amount)
            .context("transaction outputs overflow")
    })?;
    let required = output_total
        .checked_add(transaction.fee())
        .context("transaction outputs plus fee overflow")?
        .checked_add(match transaction {
            Transaction::Burn { amount, .. } => *amount,
            Transaction::Transfer { .. } | Transaction::Mine { .. } => 0,
        })
        .context("transaction outputs plus burn overflow")?;
    if input_total != required {
        bail!("transaction inputs do not balance outputs, burn, and fee");
    }
    ensure_outputs_do_not_overflow(utxos, &outputs)?;
    for (index, output) in outputs.iter().enumerate() {
        utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
    Ok(())
}

pub(super) fn validate_block_fee_policy(block: &Block) -> Result<()> {
    for transaction in &block.transactions {
        if transaction.fee() == 0 {
            bail!("block transaction fee must be greater than zero");
        }
    }
    Ok(())
}

pub(super) fn fee_reward(transactions: &[Transaction]) -> Result<Amount> {
    transactions.iter().try_fold(0_u64, |total, tx| {
        total.checked_add(tx.fee()).context("block fees overflow")
    })
}

pub(super) fn block_reward(
    transactions: &[Transaction],
    additional_finalizer_fees: Amount,
) -> Result<Amount> {
    fee_reward(transactions)?
        .checked_add(additional_finalizer_fees)
        .context("block reward overflow")
}

pub(super) fn spend_inputs(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for input in transaction.inputs() {
        if !seen.insert(input.outpoint.clone()) {
            bail!("duplicate input in transaction");
        }
        let output = utxos.remove(&input.outpoint).with_context(|| {
            format!("transaction spends missing output {}", input.outpoint.id())
        })?;
        if output.address != input.owner {
            bail!("transaction input owner does not match spent output");
        }
        total = total
            .checked_add(output.amount)
            .context("transaction input total overflows")?;
    }
    Ok(total)
}

pub(super) fn apply_spendable_pending_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    if matches!(transaction, Transaction::Mine { .. }) {
        bail!("pending mine outputs are not spendable");
    }
    transaction.verify_signature()?;
    ensure_single_input_owner(transaction)?;
    let input_total = transaction_input_total(transaction, utxos)?;
    let outputs = transaction.outputs();
    let output_total = outputs.iter().try_fold(0_u64, |total, output| {
        total
            .checked_add(output.amount)
            .context("transaction outputs overflow")
    })?;
    let required = output_total
        .checked_add(transaction.fee())
        .context("transaction outputs plus fee overflow")?
        .checked_add(match transaction {
            Transaction::Burn { amount, .. } => *amount,
            Transaction::Transfer { .. } | Transaction::Mine { .. } => 0,
        })
        .context("transaction outputs plus burn overflow")?;
    if input_total != required {
        bail!("transaction inputs do not balance outputs, burn, and fee");
    }
    ensure_outputs_do_not_overflow(utxos, &outputs)?;
    for input in transaction.inputs() {
        utxos.remove(&input.outpoint);
    }
    for (index, output) in outputs.iter().enumerate() {
        utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
    Ok(())
}

pub(super) fn transaction_input_total(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for input in transaction.inputs() {
        if !seen.insert(input.outpoint.clone()) {
            bail!("duplicate input in transaction");
        }
        let output = utxos.get(&input.outpoint).with_context(|| {
            format!("transaction spends missing output {}", input.outpoint.id())
        })?;
        if output.address != input.owner {
            bail!("transaction input owner does not match spent output");
        }
        total = total
            .checked_add(output.amount)
            .context("transaction input total overflows")?;
    }
    Ok(total)
}

pub(super) fn transaction_has_missing_inputs(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs()
        .iter()
        .any(|input| !utxos.contains_key(&input.outpoint))
}

pub(super) fn ensure_single_input_owner(transaction: &Transaction) -> Result<()> {
    if matches!(transaction, Transaction::Mine { .. }) {
        return Ok(());
    }
    ensure_single_input_owner_for_inputs(transaction.inputs())
}

pub(super) fn ensure_single_input_owner_for_inputs(inputs: &[TxInput]) -> Result<()> {
    let Some(first) = inputs.first() else {
        bail!("transaction has no inputs");
    };
    if inputs.iter().any(|input| input.owner != first.owner) {
        bail!("transaction inputs must have one owner");
    }
    Ok(())
}

pub(super) fn credit_reward_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    block: &Block,
    committee: &[BurnCommitteeMember],
) -> Result<()> {
    let outputs = reward_outputs_for_block(block, committee);
    let tx_outputs = outputs
        .iter()
        .map(|(_, output)| output.clone())
        .collect::<Vec<_>>();
    ensure_outputs_do_not_overflow(utxos, &tx_outputs)?;
    for (outpoint, output) in outputs {
        utxos.insert(outpoint, output);
    }
    Ok(())
}

pub(super) fn credit_reward_output(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    block: &Block,
) -> Result<()> {
    credit_reward_outputs(utxos, block, &[])
}

pub fn reward_outputs_for_block(
    block: &Block,
    committee: &[BurnCommitteeMember],
) -> Vec<(OutPoint, TxOutput)> {
    if block.reward == 0 {
        return Vec::new();
    }

    let mut outputs = Vec::new();
    let committee_slots = reward_committee_slots(block);
    let committee_members = committee_slots
        .into_iter()
        .filter_map(|slot| {
            committee
                .iter()
                .find(|member| member.slot == slot && member.owner != block.miner)
        })
        .collect::<Vec<_>>();
    let committee_pool = if committee_members.is_empty() {
        0
    } else {
        block.reward / 2
    };
    let finalizer_amount = block.reward.saturating_sub(committee_pool);

    if finalizer_amount > 0 {
        outputs.push((
            reward_outpoint(&block.hash),
            TxOutput {
                address: block.miner.clone(),
                amount: finalizer_amount,
            },
        ));
    }

    let mut remaining = committee_pool;
    for (index, member) in committee_members.iter().enumerate() {
        let members_left = committee_members.len() - index;
        let amount = if members_left == 1 {
            remaining
        } else {
            remaining / members_left as u64
        };
        remaining = remaining.saturating_sub(amount);
        if amount == 0 {
            continue;
        }
        outputs.push((
            committee_reward_outpoint(&block.hash, member.slot),
            TxOutput {
                address: member.owner.clone(),
                amount,
            },
        ));
    }

    outputs
}

fn reward_committee_slots(block: &Block) -> Vec<u8> {
    match block.finalizer_mode {
        FinalizerMode::Ticket if block.finalizer_rank == 0 => vec![1, 2],
        FinalizerMode::Ticket if block.finalizer_rank == 1 => vec![1],
        FinalizerMode::Ticket | FinalizerMode::Recovery => Vec::new(),
    }
}

pub(super) fn ensure_outputs_do_not_overflow(
    utxos: &BTreeMap<OutPoint, TxOutput>,
    outputs: &[TxOutput],
) -> Result<()> {
    let mut balances = BTreeMap::new();
    for output in utxos.values() {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance
            .checked_add(output.amount)
            .with_context(|| format!("balance overflow for {}", output.address))?;
    }
    for output in outputs {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance
            .checked_add(output.amount)
            .with_context(|| format!("balance overflow for {}", output.address))?;
    }
    Ok(())
}

pub(super) fn ensure_block_has_burn(transactions: &[Transaction]) -> Result<()> {
    if !transactions.iter().any(Transaction::is_burn) {
        bail!("block must include at least one burn transaction");
    }
    Ok(())
}

pub(super) fn ensure_block_has_burn_from(transactions: &[Transaction], miner: &str) -> Result<()> {
    if !transactions
        .iter()
        .any(|transaction| transaction.is_burn() && transaction.sender() == miner)
    {
        bail!("recovery block must include a burn from the finalizer");
    }
    Ok(())
}

pub(super) fn ensure_valid_recovery_block(block: &Block, parent: &Block) -> Result<()> {
    if block.finalizer_rank != 0 {
        bail!("recovery block finalizer rank must be 0");
    }
    if block.leader_proof.is_some() {
        bail!("recovery block must not carry a leader proof");
    }
    let min_timestamp = parent.timestamp_ms.saturating_add(RECOVERY_BLOCK_DELAY_MS);
    if block.timestamp_ms < min_timestamp {
        bail!("recovery block is not available before timestamp {min_timestamp}");
    }
    ensure_block_has_burn_from(&block.transactions, &block.miner)
}

pub(super) fn best_selectable_transaction_index(
    transactions: &[Transaction],
    utxos: &BTreeMap<OutPoint, TxOutput>,
    required_kind: Option<TransactionKind>,
) -> Option<usize> {
    transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| match required_kind {
            Some(TransactionKind::Burn) => tx.is_burn(),
            None => true,
        })
        .filter(|(_, tx)| {
            let mut utxos = utxos.clone();
            apply_transaction(tx, &mut utxos).is_ok()
        })
        .max_by(|(_, left), (_, right)| {
            fee_rate_key(left)
                .cmp(&fee_rate_key(right))
                .then_with(|| left.fee().cmp(&right.fee()))
                .then_with(|| left.is_burn().cmp(&right.is_burn()))
                .then_with(|| right.signature().cmp(left.signature()))
        })
        .map(|(index, _)| index)
}

pub(super) fn best_selectable_burn_from_index(
    transactions: &[Transaction],
    utxos: &BTreeMap<OutPoint, TxOutput>,
    owner: &str,
) -> Option<usize> {
    transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| tx.is_burn() && tx.sender() == owner)
        .filter(|(_, tx)| {
            let mut utxos = utxos.clone();
            apply_transaction(tx, &mut utxos).is_ok()
        })
        .max_by(|(_, left), (_, right)| {
            fee_rate_key(left)
                .cmp(&fee_rate_key(right))
                .then_with(|| left.fee().cmp(&right.fee()))
                .then_with(|| right.signature().cmp(left.signature()))
        })
        .map(|(index, _)| index)
}

pub(super) fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
}

fn committee_reward_outpoint(block_hash: &str, slot: u8) -> OutPoint {
    let slot = usize::from(slot).min(BURN_COMMITTEE_SIZE.saturating_sub(1));
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX.saturating_sub(slot as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::BurnBundleSection;

    fn reward_block(finalizer_mode: FinalizerMode, finalizer_rank: u32, reward: Amount) -> Block {
        let mut block = Block {
            height: 1,
            prev_hash: "p".repeat(64),
            timestamp_ms: 1,
            miner: "finalizer".to_string(),
            finalizer_mode,
            finalizer_rank,
            reward,
            vdf_rounds: 1,
            vdf_output: "out".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            hash: "h".repeat(64),
        };
        block.hash = block.compute_hash();
        block
    }

    fn committee_member(slot: u8, owner: &str) -> BurnCommitteeMember {
        BurnCommitteeMember {
            slot,
            root: format!("root-{slot}"),
            owner: owner.to_string(),
            weight: 1,
        }
    }

    fn output_amount(outputs: &[(OutPoint, TxOutput)], owner: &str) -> Amount {
        outputs
            .iter()
            .filter(|(_, output)| output.address == owner)
            .map(|(_, output)| output.amount)
            .sum()
    }

    #[test]
    fn rank_zero_splits_half_to_two_extra_committee_members() {
        let block = reward_block(FinalizerMode::Ticket, 0, 100);
        let committee = vec![
            committee_member(0, "finalizer"),
            committee_member(1, "committee-2"),
            committee_member(2, "committee-3"),
        ];

        let outputs = reward_outputs_for_block(&block, &committee);

        assert_eq!(output_amount(&outputs, "finalizer"), 50);
        assert_eq!(output_amount(&outputs, "committee-2"), 25);
        assert_eq!(output_amount(&outputs, "committee-3"), 25);
        assert!(
            outputs
                .iter()
                .any(|(outpoint, _)| outpoint.index == u32::MAX)
        );
        assert!(
            outputs
                .iter()
                .any(|(outpoint, _)| outpoint.index == u32::MAX - 1)
        );
        assert!(
            outputs
                .iter()
                .any(|(outpoint, _)| outpoint.index == u32::MAX - 2)
        );
    }

    #[test]
    fn rank_zero_gives_committee_half_to_the_only_available_extra_member() {
        let block = reward_block(FinalizerMode::Ticket, 0, 101);
        let committee = vec![
            committee_member(0, "finalizer"),
            committee_member(1, "committee-2"),
        ];

        let outputs = reward_outputs_for_block(&block, &committee);

        assert_eq!(output_amount(&outputs, "finalizer"), 51);
        assert_eq!(output_amount(&outputs, "committee-2"), 50);
    }

    #[test]
    fn rank_one_splits_only_with_committee_slot_one() {
        let block = reward_block(FinalizerMode::Ticket, 1, 100);
        let committee = vec![
            committee_member(0, "missed-primary"),
            committee_member(1, "committee-2"),
            committee_member(2, "committee-3"),
        ];

        let outputs = reward_outputs_for_block(&block, &committee);

        assert_eq!(output_amount(&outputs, "finalizer"), 50);
        assert_eq!(output_amount(&outputs, "committee-2"), 50);
        assert_eq!(output_amount(&outputs, "committee-3"), 0);
        assert_eq!(output_amount(&outputs, "missed-primary"), 0);
    }

    #[test]
    fn rank_two_and_recovery_pay_the_finalizer_only() {
        let committee = vec![
            committee_member(1, "committee-2"),
            committee_member(2, "committee-3"),
        ];

        let rank_two = reward_block(FinalizerMode::Ticket, 2, 100);
        let recovery = reward_block(FinalizerMode::Recovery, 0, 100);

        assert_eq!(
            output_amount(
                &reward_outputs_for_block(&rank_two, &committee),
                "finalizer"
            ),
            100
        );
        assert_eq!(reward_outputs_for_block(&rank_two, &committee).len(), 1);
        assert_eq!(
            output_amount(
                &reward_outputs_for_block(&recovery, &committee),
                "finalizer"
            ),
            100
        );
        assert_eq!(reward_outputs_for_block(&recovery, &committee).len(), 1);
    }
}
