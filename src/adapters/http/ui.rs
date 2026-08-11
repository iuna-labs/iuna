use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    Amount, BLINDED_COMMITTER_FEE_BPS, BLINDED_FEE_BPS_DENOMINATOR,
    BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS, BlindedReveal, BlindedTransaction, Block, BurnLeaderRank,
    ChainSnapshot, Ledger, MINE_REWARD, OutPoint, REVEAL_COMMITTEE_SIZE,
    RevealedBlindedTransaction, Transaction, TxInput, TxOutput, blinded_reveal_finalizer_fee,
    hex_hash, revealed_blinded_transactions,
};

use super::{
    HttpState, UiChainView,
    types::{
        UiBlock, UiByteBreakdown, UiRevealBundle, UiTransaction, UiTxInput,
        WalletTransactionContext, WalletTransactionFilters, WalletTransactionRow,
    },
};

pub(super) fn wallet_transaction_rows(
    wallet: &str,
    pending: Vec<Transaction>,
    owned_blinded: Vec<Transaction>,
    chain: &[Block],
    revealed_by_height: &BTreeMap<u64, Vec<RevealedBlindedTransaction>>,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    filters: WalletTransactionFilters,
) -> Vec<WalletTransactionRow> {
    let mut rows = Vec::new();
    let pending_context = WalletTransactionContext {
        status: "pending",
        block_height: None,
        timestamp_ms: None,
        block_finalizer: None,
        blinded: false,
    };

    for (index, tx) in pending.iter().enumerate() {
        if !filters.allows(tx) {
            continue;
        }
        if let Some(row) = wallet_transaction_row(wallet, tx, outputs, &pending_context) {
            rows.push((u128::MAX - index as u128, row));
        }
    }

    let pending_blind_context = WalletTransactionContext {
        blinded: true,
        ..pending_context
    };
    for (index, tx) in owned_blinded.iter().enumerate() {
        if !filters.allows(tx) {
            continue;
        }
        if let Some(row) = wallet_transaction_row(wallet, tx, outputs, &pending_blind_context) {
            rows.push((u128::MAX - 10_000 - index as u128, row));
        }
    }

    for block in chain {
        for (index, tx) in block.transactions.iter().rev().enumerate() {
            if !filters.allows(tx) {
                continue;
            }
            if let Some(row) = wallet_transaction_row(
                wallet,
                tx,
                outputs,
                &WalletTransactionContext {
                    status: "confirmed",
                    block_height: Some(block.height),
                    timestamp_ms: Some(block.timestamp_ms),
                    block_finalizer: Some(block.miner.clone()),
                    blinded: false,
                },
            ) {
                rows.push((block.height as u128 * 10_000 + index as u128, row));
            }
        }
        if let Some(revealed_transactions) = revealed_by_height.get(&block.height) {
            for (index, revealed) in revealed_transactions.iter().rev().enumerate() {
                let tx = &revealed.transaction;
                if !filters.allows(tx) {
                    continue;
                }
                if let Some(row) = wallet_transaction_row(
                    wallet,
                    tx,
                    outputs,
                    &WalletTransactionContext {
                        status: "confirmed",
                        block_height: Some(block.height),
                        timestamp_ms: Some(block.timestamp_ms),
                        block_finalizer: Some(block.miner.clone()),
                        blinded: false,
                    },
                ) {
                    rows.push((block.height as u128 * 10_000 + 5_000 + index as u128, row));
                }
            }
        }
    }

    rows.sort_by(|left, right| right.0.cmp(&left.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

fn wallet_transaction_row(
    wallet: &str,
    tx: &Transaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
    context: &WalletTransactionContext,
) -> Option<WalletTransactionRow> {
    match tx {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } if tx.sender() == wallet || tx.to() == Some(wallet) => Some(WalletTransactionRow {
            kind: "transfer",
            from: tx.sender().to_string(),
            to: tx.to().map(str::to_string),
            amount: tx.amount(),
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: outputs.clone(),
            change: Vec::new(),
            signature: signature.clone(),
            status: context.status,
            block_height: context.block_height,
            timestamp_ms: context.timestamp_ms,
            block_finalizer: context.block_finalizer.clone(),
            direction: if tx.to() == Some(wallet) {
                "received"
            } else {
                "sent"
            },
            blinded: context.blinded,
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } if tx.sender() == wallet => Some(WalletTransactionRow {
            kind: "burn",
            from: tx.sender().to_string(),
            to: None,
            amount: *amount,
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: Vec::new(),
            change: change.clone(),
            signature: signature.clone(),
            status: context.status,
            block_height: context.block_height,
            timestamp_ms: context.timestamp_ms,
            block_finalizer: context.block_finalizer.clone(),
            direction: "burned",
            blinded: context.blinded,
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        Transaction::Mine {
            recipient,
            difficulty_bits,
            signature,
            ..
        } if recipient == wallet => Some(WalletTransactionRow {
            kind: "mine",
            from: "pow".to_string(),
            to: Some(recipient.clone()),
            amount: MINE_REWARD,
            fee: tx.fee(),
            inputs: Vec::new(),
            outputs: vec![TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            }],
            change: Vec::new(),
            signature: signature.clone(),
            status: context.status,
            block_height: context.block_height,
            timestamp_ms: context.timestamp_ms,
            block_finalizer: context.block_finalizer.clone(),
            direction: "received",
            blinded: context.blinded,
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
        }),
        _ => None,
    }
}

pub(super) fn revealed_transactions_by_height(
    snapshot: &ChainSnapshot,
) -> BTreeMap<u64, Vec<RevealedBlindedTransaction>> {
    revealed_blinded_transactions(snapshot)
        .unwrap_or_default()
        .into_iter()
        .fold(
            BTreeMap::<u64, Vec<RevealedBlindedTransaction>>::new(),
            |mut by_height, revealed| {
                by_height.entry(revealed.height).or_default().push(revealed);
                by_height
            },
        )
}

#[cfg(test)]
pub(super) fn ui_blocks(
    blocks: Vec<Block>,
    snapshot: &ChainSnapshot,
    pending: &[Transaction],
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
) -> Vec<UiBlock> {
    let outputs = known_output_index(snapshot, pending);
    let revealed = revealed_transactions_by_height(snapshot);
    ui_blocks_from_indexes(blocks, &outputs, &revealed, burn_leader_ranks)
}

pub(super) fn ui_blocks_from_indexes(
    blocks: Vec<Block>,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    revealed: &BTreeMap<u64, Vec<RevealedBlindedTransaction>>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
) -> Vec<UiBlock> {
    blocks
        .into_iter()
        .map(|block| {
            let revealed_transactions = revealed.get(&block.height).cloned().unwrap_or_default();
            ui_block(block, outputs, burn_leader_ranks, &revealed_transactions)
        })
        .collect()
}

pub(super) fn ui_block(
    block: Block,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
    revealed_transactions: &[RevealedBlindedTransaction],
) -> UiBlock {
    let ranks = burn_leader_ranks
        .get(&block.hash)
        .cloned()
        .unwrap_or_default();
    let revealed_fees = revealed_transactions.iter().fold(0_u64, |total, revealed| {
        total.saturating_add(revealed.transaction.fee())
    });
    let transaction_bytes = block
        .transactions
        .iter()
        .map(|tx| tx.serialized_size_bytes().unwrap_or_default())
        .sum::<usize>();
    let transaction_byte_breakdown = transaction_byte_breakdown(&block.transactions);
    let blinded_transaction_bytes = block
        .blinded_transactions
        .iter()
        .map(|tx| tx.serialized_size_bytes().unwrap_or_default())
        .sum::<usize>();
    let mut transactions = block
        .transactions
        .iter()
        .map(|tx| ui_transaction(tx, outputs))
        .collect::<Vec<_>>();
    transactions.extend(
        block
            .blinded_transactions
            .iter()
            .map(|transaction| ui_blinded_transaction(transaction, outputs)),
    );
    transactions.extend(
        revealed_transactions
            .iter()
            .map(|revealed| ui_revealed_transaction(&revealed.transaction, outputs)),
    );
    let revealed_by_commitment = revealed_transactions
        .iter()
        .map(|revealed| (revealed.commitment.clone(), revealed.transaction.clone()))
        .collect::<BTreeMap<_, _>>();
    let reveal_bundles: Vec<UiRevealBundle> = block
        .reveal_bundle_section
        .expand(block.height, &block.prev_hash)
        .into_iter()
        .map(|bundle| UiRevealBundle {
            slot: bundle.slot,
            member: bundle.member.clone(),
            hash: bundle.bundle_hash(),
            byte_size: bundle.serialized_size_bytes().unwrap_or_default(),
            reveals: bundle
                .reveals
                .iter()
                .map(|reveal| {
                    revealed_by_commitment
                        .get(&reveal.commitment)
                        .map(|tx| ui_revealed_transaction(tx, outputs))
                        .unwrap_or_else(|| ui_blinded_reveal(reveal))
                })
                .collect(),
        })
        .collect();
    let reveal_bundle_bytes = reveal_bundles
        .iter()
        .map(|bundle: &UiRevealBundle| bundle.byte_size)
        .sum::<usize>();
    let total_bytes = block.serialized_size_bytes().unwrap_or_else(|_| {
        transaction_bytes
            .saturating_add(blinded_transaction_bytes)
            .saturating_add(reveal_bundle_bytes)
    });
    UiBlock {
        height: block.height,
        prev_hash: block.prev_hash,
        timestamp_ms: block.timestamp_ms,
        miner: block.miner,
        finalizer_mode: block.finalizer_mode,
        finalizer_rank: block.finalizer_rank,
        reward: block.reward,
        total_fees: block.reward.saturating_add(revealed_fees),
        total_bytes,
        transaction_bytes,
        transaction_byte_breakdown,
        blinded_transaction_bytes,
        reveal_bundle_bytes,
        vdf_rounds: block.vdf_rounds,
        vdf_output: block.vdf_output,
        leader_proof: block.leader_proof,
        burn_leader_ranks: ranks,
        transactions,
        revealed_transactions: revealed_transactions
            .iter()
            .map(|revealed| ui_revealed_transaction(&revealed.transaction, outputs))
            .collect(),
        reveal_bundles,
        hash: block.hash,
    }
}

fn ui_revealed_transaction(
    transaction: &Transaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> UiTransaction {
    let mut row = ui_transaction(transaction, outputs_by_outpoint);
    row.revealed = true;
    row
}

fn transaction_byte_breakdown(transactions: &[Transaction]) -> Vec<UiByteBreakdown> {
    let mut transfer_bytes = 0_usize;
    let mut burn_bytes = 0_usize;
    let mut mine_bytes = 0_usize;
    for transaction in transactions {
        let bytes = transaction.serialized_size_bytes().unwrap_or_default();
        match transaction {
            Transaction::Transfer { .. } => transfer_bytes = transfer_bytes.saturating_add(bytes),
            Transaction::Burn { .. } => burn_bytes = burn_bytes.saturating_add(bytes),
            Transaction::Mine { .. } => mine_bytes = mine_bytes.saturating_add(bytes),
        }
    }
    [
        ("transfer", transfer_bytes),
        ("burn", burn_bytes),
        ("mine", mine_bytes),
    ]
    .into_iter()
    .filter_map(|(label, bytes)| (bytes > 0).then_some(UiByteBreakdown { label, bytes }))
    .collect()
}

pub(super) fn ui_pending_revealed_transaction(
    revealed: &RevealedBlindedTransaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> UiTransaction {
    let mut row = ui_revealed_transaction(&revealed.transaction, outputs_by_outpoint);
    row.commitment = Some(revealed.commitment.clone());
    row
}

pub(super) fn ui_transaction(
    transaction: &Transaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> UiTransaction {
    match transaction {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } => UiTransaction {
            kind: "transfer",
            from: transaction.sender().to_string(),
            to: transaction.to().map(str::to_string),
            amount: transaction.amount(),
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: outputs.clone(),
            change: Vec::new(),
            signature: signature.clone(),
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
            revealed: false,
        },
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } => UiTransaction {
            kind: "burn",
            from: transaction.sender().to_string(),
            to: None,
            amount: *amount,
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: Vec::new(),
            change: change.clone(),
            signature: signature.clone(),
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
            revealed: false,
        },
        Transaction::Mine {
            recipient,
            difficulty_bits,
            signature,
            ..
        } => UiTransaction {
            kind: "mine",
            from: "pow".to_string(),
            to: Some(recipient.clone()),
            amount: MINE_REWARD,
            fee: transaction.fee(),
            inputs: Vec::new(),
            outputs: vec![TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            }],
            change: Vec::new(),
            signature: signature.clone(),
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
            revealed: false,
        },
    }
}

pub(super) fn ui_blinded_transaction(
    transaction: &BlindedTransaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> UiTransaction {
    UiTransaction {
        kind: "blinded",
        from: transaction
            .inputs
            .first()
            .map(|input| input.owner.clone())
            .unwrap_or_else(|| "encrypted".to_string()),
        to: None,
        amount: 0,
        fee: transaction.fee,
        inputs: ui_inputs(&transaction.inputs, outputs_by_outpoint),
        outputs: Vec::new(),
        change: Vec::new(),
        signature: transaction.commitment.clone(),
        difficulty_bits: None,
        proof_bits: None,
        proof_hash: None,
        commitment: Some(transaction.commitment.clone()),
        encrypted_size: Some(transaction.encrypted_size),
        expires_at_height: Some(transaction.expires_at_height),
        revealed: false,
    }
}

pub(super) fn ui_blinded_reveal(reveal: &BlindedReveal) -> UiTransaction {
    UiTransaction {
        kind: "reveal",
        from: "encrypted".to_string(),
        to: None,
        amount: 0,
        fee: 0,
        inputs: Vec::new(),
        outputs: Vec::new(),
        change: Vec::new(),
        signature: reveal.commitment.clone(),
        difficulty_bits: None,
        proof_bits: None,
        proof_hash: None,
        commitment: Some(reveal.commitment.clone()),
        encrypted_size: None,
        expires_at_height: None,
        revealed: false,
    }
}

fn ui_inputs(
    inputs: &[TxInput],
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> Vec<UiTxInput> {
    inputs
        .iter()
        .map(|input| {
            let spent_output = outputs_by_outpoint.get(&input.outpoint);
            UiTxInput {
                outpoint: input.outpoint.clone(),
                owner: input.owner.clone(),
                signature: input.signature.clone(),
                amount: spent_output.map(|output| output.amount),
                address: spent_output.map(|output| output.address.clone()),
            }
        })
        .collect()
}

fn proof_bits(hex_hash: &str) -> u32 {
    let mut bits = 0_u32;
    for byte in hex_hash.as_bytes() {
        let Some(nibble) = hex_nibble(*byte) else {
            break;
        };
        if nibble == 0 {
            bits += 4;
            continue;
        }
        bits += nibble.leading_zeros() - 4;
        break;
    }
    bits
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn known_output_index(
    snapshot: &ChainSnapshot,
    pending: &[Transaction],
) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = known_chain_output_index(snapshot);
    add_pending_outputs(&mut outputs, pending);
    outputs
}

pub(super) async fn cached_chain_view(state: &HttpState, snapshot: &ChainSnapshot) -> UiChainView {
    let tip_hash = snapshot.blocks.last().map(|block| block.hash.clone());
    {
        let cache = state.ui_cache.lock().await;
        if cache.tip_hash == tip_hash {
            return UiChainView {
                outputs: cache.outputs.clone(),
                revealed_by_height: cache.revealed_by_height.clone(),
            };
        }
    }

    let outputs = known_chain_output_index(snapshot);
    let revealed_by_height = revealed_transactions_by_height(snapshot);

    let mut cache = state.ui_cache.lock().await;
    if cache.tip_hash == tip_hash {
        return UiChainView {
            outputs: cache.outputs.clone(),
            revealed_by_height: cache.revealed_by_height.clone(),
        };
    }

    let view = UiChainView {
        outputs,
        revealed_by_height,
    };
    cache.tip_hash = tip_hash;
    cache.outputs = view.outputs.clone();
    cache.revealed_by_height = view.revealed_by_height.clone();
    UiChainView {
        outputs: view.outputs,
        revealed_by_height: view.revealed_by_height,
    }
}

fn known_chain_output_index(snapshot: &ChainSnapshot) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = BTreeMap::new();
    for (address, amount) in &snapshot.genesis_allocations {
        if *amount == 0 {
            continue;
        }
        outputs.insert(
            genesis_allocation_outpoint(address),
            TxOutput {
                address: address.clone(),
                amount: *amount,
            },
        );
    }
    let revealed = revealed_blinded_transactions(snapshot).unwrap_or_default();
    let blocks_by_height = snapshot
        .blocks
        .iter()
        .map(|block| (block.height, block))
        .collect::<BTreeMap<_, _>>();
    let reveal_bundle_slots_by_height = Ledger::from_persisted_snapshot(snapshot.clone())
        .ok()
        .map(|ledger| {
            snapshot
                .blocks
                .iter()
                .map(|block| {
                    let slots = ledger
                        .burn_leader_ranks_for_block(block.height)
                        .map(|ranks| ranks.len())
                        .unwrap_or(REVEAL_COMMITTEE_SIZE);
                    (block.height, slots)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let blinded_by_commitment = snapshot
        .blocks
        .iter()
        .flat_map(|block| block.blinded_transactions.iter())
        .map(|transaction| (transaction.commitment.clone(), transaction.clone()))
        .collect::<BTreeMap<_, _>>();
    for block in &snapshot.blocks {
        for transaction in &block.transactions {
            index_transaction_outputs(&mut outputs, transaction);
        }
        if block.reward > 0 {
            outputs.insert(
                reward_outpoint(&block.hash),
                TxOutput {
                    address: block.miner.clone(),
                    amount: block.reward,
                },
            );
        }
    }
    for revealed in revealed {
        index_transaction_outputs(&mut outputs, &revealed.transaction);
        let fee = revealed.transaction.fee();
        if matches!(revealed.transaction, Transaction::Mine { .. }) {
            if let Some(commit) = blinded_by_commitment.get(&revealed.commitment) {
                index_blinded_collateral_change(&mut outputs, commit, fee);
            }
        }
        if fee > 0 {
            let committer_fee = blinded_fee_share(fee, BLINDED_COMMITTER_FEE_BPS);
            if committer_fee > 0 {
                outputs.insert(
                    blinded_committer_fee_outpoint(&revealed.commitment),
                    TxOutput {
                        address: revealed.included_by,
                        amount: committer_fee,
                    },
                );
            }
            if let Some(block) = blocks_by_height.get(&revealed.height) {
                let reveal_finalizer_fee = blinded_reveal_finalizer_fee(
                    fee,
                    block.included_reveal_bundle_count(),
                    reveal_bundle_slots_by_height
                        .get(&revealed.height)
                        .copied()
                        .unwrap_or(REVEAL_COMMITTEE_SIZE),
                );
                if reveal_finalizer_fee > 0 {
                    outputs.insert(
                        blinded_executor_fee_outpoint(&revealed.commitment),
                        TxOutput {
                            address: block.miner.clone(),
                            amount: reveal_finalizer_fee,
                        },
                    );
                }
                let reveal_bundle_signer_fee =
                    blinded_fee_share(fee, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS);
                if reveal_bundle_signer_fee > 0 {
                    for signature in &block.reveal_bundle_section.signatures {
                        outputs.insert(
                            blinded_reveal_bundle_signer_fee_outpoint(
                                &revealed.commitment,
                                signature.slot,
                            ),
                            TxOutput {
                                address: signature.member.clone(),
                                amount: reveal_bundle_signer_fee,
                            },
                        );
                    }
                }
            }
        }
    }
    index_expired_blinded_outputs(&mut outputs, snapshot);
    outputs
}

pub(super) fn add_pending_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    pending: &[Transaction],
) {
    for transaction in pending {
        index_transaction_outputs(outputs, transaction);
    }
}

fn index_blinded_collateral_change(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &BlindedTransaction,
    fee: Amount,
) {
    let Some(first_input) = transaction.inputs.first() else {
        return;
    };
    let locked_total = transaction.inputs.iter().fold(0_u64, |total, input| {
        total.saturating_add(
            outputs
                .get(&input.outpoint)
                .map(|output| output.amount)
                .unwrap_or_default(),
        )
    });
    if fee >= locked_total {
        return;
    }
    outputs.insert(
        blinded_expiry_change_outpoint(&transaction.commitment),
        TxOutput {
            address: first_input.owner.clone(),
            amount: locked_total - fee,
        },
    );
}

fn index_expired_blinded_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    snapshot: &ChainSnapshot,
) {
    let mut active = BTreeMap::<String, (BlindedTransaction, Amount)>::new();
    for block in &snapshot.blocks {
        let revealed = block
            .all_blinded_reveals()
            .into_iter()
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();
        active.retain(|commitment, (transaction, locked_total)| {
            if revealed.contains(commitment) {
                return false;
            }
            if block.height >= transaction.expires_at_height {
                if let Some(first_input) = transaction.inputs.first() {
                    if transaction.fee <= *locked_total {
                        let change = *locked_total - transaction.fee;
                        if change > 0 {
                            outputs.insert(
                                blinded_expiry_change_outpoint(commitment),
                                TxOutput {
                                    address: first_input.owner.clone(),
                                    amount: change,
                                },
                            );
                        }
                    }
                }
                return false;
            }
            true
        });
        for transaction in &block.blinded_transactions {
            let locked_total = transaction.inputs.iter().fold(0_u64, |total, input| {
                total.saturating_add(
                    outputs
                        .get(&input.outpoint)
                        .map(|output| output.amount)
                        .unwrap_or_default(),
                )
            });
            active.insert(
                transaction.commitment.clone(),
                (transaction.clone(), locked_total),
            );
        }
    }
}

fn index_transaction_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &Transaction,
) {
    let created_outputs = match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        }],
    };
    for (index, output) in created_outputs.iter().enumerate() {
        outputs.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
}

fn genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
}

fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
}

fn blinded_committer_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

fn blinded_executor_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 2,
    }
}

fn blinded_reveal_bundle_signer_fee_outpoint(commitment: &str, slot: u8) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 3 - u32::from(slot),
    }
}

fn blinded_expiry_change_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: 0,
    }
}

fn blinded_fee_share(fee: Amount, bps: u64) -> Amount {
    ((fee as u128 * bps as u128) / BLINDED_FEE_BPS_DENOMINATOR as u128) as Amount
}
