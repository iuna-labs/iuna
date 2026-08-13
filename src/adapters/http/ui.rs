use std::collections::BTreeMap;

use crate::domain::{
    BLINDED_FEE_BPS_DENOMINATOR, BLINDED_REVEAL_FINALIZER_FEE_BPS, BlindedReveal,
    BlindedTransaction, Block, BurnLeaderRank, ChainSnapshot, MINE_REWARD, OutPoint,
    REVEAL_COMMITTEE_SIZE, RevealedBlindedTransaction, Transaction, TxInput, TxOutput,
    blinded_reveal_finalizer_fee, reveal_committee_slot_count,
};

use crate::adapters::ui_index::build_ui_chain_index;

use super::{
    HttpState, UiChainView,
    types::{
        UiBlock, UiByteBreakdown, UiRevealBundle, UiRevealFeePenalty, UiTransaction, UiTxInput,
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

pub(super) fn wallet_transaction_row(
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

#[cfg(test)]
pub(super) fn revealed_transactions_by_height(
    snapshot: &ChainSnapshot,
) -> BTreeMap<u64, Vec<RevealedBlindedTransaction>> {
    crate::adapters::ui_index::revealed_transactions_by_height(snapshot)
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
    let reveal_lists_included = block.included_reveal_bundle_count();
    let committee_size = if ranks.is_empty() {
        REVEAL_COMMITTEE_SIZE
    } else {
        reveal_committee_slot_count(ranks.len())
    };
    let revealed_fees = revealed_transactions.iter().fold(0_u64, |total, revealed| {
        total.saturating_add(revealed.transaction.fee())
    });
    let reveal_fee_penalty =
        reveal_fee_penalty(revealed_transactions, reveal_lists_included, committee_size);
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
        reveal_fee_penalty,
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

fn reveal_fee_penalty(
    revealed_transactions: &[RevealedBlindedTransaction],
    reveal_lists_included: usize,
    committee_size: usize,
) -> UiRevealFeePenalty {
    let fee_penalty = revealed_transactions.iter().fold(0_u64, |total, revealed| {
        let fee = revealed.transaction.fee();
        let full_finalizer_share = ((fee as u128 * BLINDED_REVEAL_FINALIZER_FEE_BPS as u128)
            / BLINDED_FEE_BPS_DENOMINATOR as u128) as u64;
        let paid_finalizer_share =
            blinded_reveal_finalizer_fee(fee, reveal_lists_included, committee_size);
        total.saturating_add(full_finalizer_share.saturating_sub(paid_finalizer_share))
    });
    UiRevealFeePenalty {
        reveal_lists_included,
        committee_size,
        fee_penalty,
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
    let mut outputs = build_ui_chain_index(snapshot).outputs;
    add_pending_outputs(&mut outputs, pending);
    outputs
}

pub(super) async fn cached_chain_view(
    state: &HttpState,
    snapshot: &ChainSnapshot,
) -> anyhow::Result<UiChainView> {
    let tip_hash = snapshot.blocks.last().map(|block| block.hash.clone());
    {
        let cache = state.ui_cache.lock().await;
        if cache.tip_hash == tip_hash {
            return Ok(ui_chain_view_from_cache(&cache));
        }
    }

    let (computed_tip_hash, view) = tokio::task::spawn_blocking({
        let snapshot = snapshot.clone();
        move || build_chain_view(&snapshot)
    })
    .await?;

    let mut cache = state.ui_cache.lock().await;
    if cache.tip_hash == tip_hash {
        return Ok(ui_chain_view_from_cache(&cache));
    }

    cache.tip_hash = computed_tip_hash;
    cache.outputs = view.outputs.clone();
    cache.revealed_by_height = view.revealed_by_height.clone();
    cache.burn_leader_ranks_by_hash = view.burn_leader_ranks_by_hash.clone();
    Ok(UiChainView {
        outputs: view.outputs,
        revealed_by_height: view.revealed_by_height,
        burn_leader_ranks_by_hash: view.burn_leader_ranks_by_hash,
    })
}

pub(super) async fn cached_ui_blocks_for_tip(
    state: &HttpState,
    tip_hash: Option<&str>,
    blocks: Vec<Block>,
) -> Option<Vec<UiBlock>> {
    let cache = state.ui_cache.lock().await;
    (cache.tip_hash.as_deref() == tip_hash).then(|| {
        ui_blocks_from_indexes(
            blocks,
            &cache.outputs,
            &cache.revealed_by_height,
            &cache.burn_leader_ranks_by_hash,
        )
    })
}

fn ui_chain_view_from_cache(cache: &super::UiChainCache) -> UiChainView {
    UiChainView {
        outputs: cache.outputs.clone(),
        revealed_by_height: cache.revealed_by_height.clone(),
        burn_leader_ranks_by_hash: cache.burn_leader_ranks_by_hash.clone(),
    }
}

fn build_chain_view(snapshot: &ChainSnapshot) -> (Option<String>, UiChainView) {
    let index = build_ui_chain_index(snapshot);
    (
        index.tip_hash.clone(),
        UiChainView {
            outputs: index.outputs,
            revealed_by_height: index.revealed_by_height,
            burn_leader_ranks_by_hash: index.burn_leader_ranks_by_hash,
        },
    )
}

pub(super) fn add_pending_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    pending: &[Transaction],
) {
    for transaction in pending {
        index_transaction_outputs(outputs, transaction);
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
