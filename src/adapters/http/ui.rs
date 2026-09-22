use std::collections::BTreeMap;

use anyhow::Result;

use crate::compact::CompactBlockSizeBreakdown;
use crate::domain::{
    AddressNetwork, Amount, Block, BurnLeaderRank, FinalizerMode, LegacyTransactionId, MINE_REWARD,
    OutPoint, Transaction, TransactionV2, TransactionV2Domain, TxInput, TxOutput, decode_hex,
    encode_versioned_address, hex_encode,
};

use super::types::{
    UiBlock, UiBurnBundle, UiBurnBundleQuorum, UiByteBreakdown, UiRewardFeeInput, UiRewardOutput,
    UiTransaction, UiTxInput, WalletTransactionContext, WalletTransactionFilters,
    WalletTransactionRow,
};

pub(super) fn wallet_transaction_rows(
    wallet: &str,
    pending: Vec<Transaction>,
    chain: &[Block],
    outputs: &BTreeMap<OutPoint, TxOutput>,
    filters: WalletTransactionFilters,
) -> Vec<WalletTransactionRow> {
    let mut rows = Vec::new();
    let pending_context = WalletTransactionContext {
        status: "pending",
        block_height: None,
        timestamp_ms: None,
        block_finalizer: None,
    };

    for (index, tx) in pending.iter().enumerate() {
        if !filters.allows(tx) {
            continue;
        }
        if let Some(row) = wallet_transaction_row(wallet, tx, outputs, &pending_context) {
            rows.push((u128::MAX - index as u128, row));
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
                },
            ) {
                rows.push((block.height as u128 * 10_000 + index as u128, row));
            }
        }
    }

    rows.sort_by(|left, right| right.0.cmp(&left.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

pub(super) fn wallet_transaction_v2_rows(
    wallet_addresses: &[String],
    pending: &[TransactionV2],
    outputs: &BTreeMap<OutPoint, TxOutput>,
    filters: WalletTransactionFilters,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
) -> Vec<WalletTransactionRow> {
    let context = WalletTransactionContext {
        status: "pending",
        block_height: None,
        timestamp_ms: None,
        block_finalizer: None,
    };
    pending
        .iter()
        .rev()
        .filter(|transaction| filters.allows_v2(transaction))
        .filter_map(|transaction| {
            wallet_transaction_v2_row(
                wallet_addresses,
                transaction,
                outputs,
                domain,
                network,
                &context,
            )
            .ok()
            .flatten()
        })
        .collect()
}

pub(super) fn wallet_transaction_v2_row(
    wallet_addresses: &[String],
    transaction: &TransactionV2,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
    context: &WalletTransactionContext,
) -> Result<Option<WalletTransactionRow>> {
    let presented = ui_transaction_v2(transaction, outputs, domain, network)?;
    let is_wallet_address =
        |candidate: &str| wallet_addresses.iter().any(|address| address == candidate);
    let sent = is_wallet_address(&presented.from);
    let received = presented
        .outputs
        .iter()
        .chain(presented.change.iter())
        .any(|output| is_wallet_address(&output.address));
    if !sent && !received {
        return Ok(None);
    }
    let amount = if matches!(transaction, TransactionV2::Migration { .. }) {
        presented.amount
    } else if sent {
        presented
            .outputs
            .iter()
            .filter(|output| !is_wallet_address(&output.address))
            .fold(0_u64, |total, output| total.saturating_add(output.amount))
    } else {
        presented
            .outputs
            .iter()
            .chain(presented.change.iter())
            .filter(|output| is_wallet_address(&output.address))
            .fold(0_u64, |total, output| total.saturating_add(output.amount))
    };
    let direction = match transaction {
        TransactionV2::Migration { .. } => "migrated",
        TransactionV2::Burn { .. } => "burned",
        _ if sent => "sent",
        _ => "received",
    };
    Ok(Some(WalletTransactionRow {
        kind: presented.kind,
        from: presented.from,
        to: presented.to,
        amount,
        fee: presented.fee,
        inputs: presented.inputs,
        outputs: presented.outputs,
        change: presented.change,
        signature: presented.signature,
        status: context.status,
        block_height: context.block_height,
        timestamp_ms: context.timestamp_ms,
        block_finalizer: context.block_finalizer.clone(),
        direction,
        difficulty_bits: presented.difficulty_bits,
        proof_bits: presented.proof_bits,
        proof_hash: presented.proof_hash,
        reward_total: None,
        reward_fee_inputs: Vec::new(),
        reward_outputs: Vec::new(),
    }))
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
        } if tx.sender() == wallet || outputs.iter().any(|output| output.address == wallet) => {
            let sent = tx.sender() == wallet;
            let amount = if sent {
                tx.amount()
            } else {
                outputs
                    .iter()
                    .filter(|output| output.address == wallet)
                    .fold(0_u64, |total, output| total.saturating_add(output.amount))
            };
            Some(WalletTransactionRow {
                kind: "transfer",
                from: tx.sender().to_string(),
                to: tx.to().map(str::to_string),
                amount,
                fee: *fee,
                inputs: ui_inputs(inputs, outputs_by_outpoint),
                outputs: outputs.clone(),
                change: Vec::new(),
                signature: signature.clone(),
                status: context.status,
                block_height: context.block_height,
                timestamp_ms: context.timestamp_ms,
                block_finalizer: context.block_finalizer.clone(),
                direction: if sent { "sent" } else { "received" },
                difficulty_bits: None,
                proof_bits: None,
                proof_hash: None,
                reward_total: None,
                reward_fee_inputs: Vec::new(),
                reward_outputs: Vec::new(),
            })
        }
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
            ..
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
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
            reward_total: None,
            reward_fee_inputs: Vec::new(),
            reward_outputs: Vec::new(),
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
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
            reward_total: None,
            reward_fee_inputs: Vec::new(),
            reward_outputs: Vec::new(),
        }),
        _ => None,
    }
}

pub(super) fn populate_wallet_reward_flow(row: &mut WalletTransactionRow, block: &Block) {
    row.reward_total = Some(block.reward);
    row.reward_fee_inputs = block
        .transactions
        .iter()
        .filter(|transaction| transaction.fee() > 0)
        .map(|transaction| UiRewardFeeInput {
            transaction_kind: transaction_kind(transaction),
            amount: transaction.fee(),
            owner: match transaction {
                Transaction::Mine { .. } => "pow".to_string(),
                _ => transaction.sender().to_string(),
            },
            signature: transaction.signature().to_string(),
        })
        .collect();

    let committee_slots = block
        .burn_bundle_section
        .signatures
        .iter()
        .filter(|signature| signature.member != block.miner)
        .map(|signature| (signature.member.as_str(), signature.slot))
        .collect::<BTreeMap<_, _>>();

    row.reward_outputs = row
        .outputs
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, output)| UiRewardOutput {
            label: if index == 0 {
                "Finalizer reward".to_string()
            } else if let Some(slot) = committee_slots.get(output.address.as_str()) {
                format!("Committee reward (slot {slot})")
            } else {
                format!("Committee reward {index}")
            },
            amount: output.amount,
            address: output.address,
        })
        .collect();
}

fn transaction_kind(transaction: &Transaction) -> &'static str {
    match transaction {
        Transaction::Transfer { .. } => "transfer",
        Transaction::Burn { .. } => "burn",
        Transaction::Mine { .. } => "mine",
    }
}

pub(super) fn ui_blocks_from_indexes(
    blocks: Vec<Block>,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
    storage_size_breakdowns: &BTreeMap<String, CompactBlockSizeBreakdown>,
    transaction_v2_domain: Option<&TransactionV2Domain>,
    network: AddressNetwork,
) -> Vec<UiBlock> {
    blocks
        .into_iter()
        .map(|block| {
            let storage_size = storage_size_breakdowns.get(&block.hash);
            ui_block_with_v2(
                block,
                outputs,
                burn_leader_ranks,
                storage_size,
                transaction_v2_domain,
                network,
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) fn ui_block(
    block: Block,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
    storage_size: Option<&CompactBlockSizeBreakdown>,
) -> UiBlock {
    ui_block_with_v2(
        block,
        outputs,
        burn_leader_ranks,
        storage_size,
        None,
        AddressNetwork::Mainnet,
    )
}

fn ui_block_with_v2(
    block: Block,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
    storage_size: Option<&CompactBlockSizeBreakdown>,
    transaction_v2_domain: Option<&TransactionV2Domain>,
    network: AddressNetwork,
) -> UiBlock {
    let ranks = burn_leader_ranks
        .get(&block.hash)
        .cloned()
        .unwrap_or_default();
    let (burn_bundles_included, burn_bundles_required) = burn_bundle_wallet_quorum(&block);
    let public_fees = block
        .transactions
        .iter()
        .fold(0_u64, |total, tx| total.saturating_add(tx.fee()));
    let transactions_v2 = transaction_v2_domain
        .map(|domain| decode_block_transactions_v2(&block, domain))
        .unwrap_or_default();
    let total_fees = transactions_v2
        .iter()
        .fold(public_fees, |total, tx| total.saturating_add(tx.fee()));
    let transaction_bytes = storage_size
        .map(|size| size.transaction_bytes)
        .unwrap_or_else(|| {
            let legacy_bytes = block
                .transactions
                .iter()
                .map(|tx| tx.serialized_size_bytes().unwrap_or_default())
                .sum::<usize>();
            let v2_bytes = transaction_v2_domain.map_or(0, |domain| {
                transactions_v2.iter().fold(0_usize, |total, transaction| {
                    total.saturating_add(transaction.encoded_size_bytes(domain).unwrap_or_default())
                })
            });
            legacy_bytes.saturating_add(v2_bytes)
        });
    let transaction_byte_breakdown = transaction_byte_breakdown(
        &block.transactions,
        &transactions_v2,
        storage_size,
        transaction_v2_domain,
    );
    let header_and_proof_bytes = storage_size
        .map(|size| size.header_and_proof_bytes)
        .unwrap_or_default();
    let mut transactions = block
        .transactions
        .iter()
        .map(|tx| ui_transaction(tx, outputs))
        .collect::<Vec<_>>();
    if let Some(domain) = transaction_v2_domain {
        transactions.extend(transactions_v2.iter().filter_map(|transaction| {
            ui_transaction_v2(transaction, outputs, domain, network).ok()
        }));
    }
    let burn_bundles: Vec<UiBurnBundle> = block
        .burn_bundle_section
        .expand(block.height, &block.prev_hash)
        .into_iter()
        .map(|bundle| UiBurnBundle {
            slot: bundle.slot,
            member: bundle.member.clone(),
            hash: bundle.bundle_hash(),
            byte_size: bundle.serialized_size_bytes().unwrap_or_default(),
            burns: bundle
                .burns
                .iter()
                .map(|burn| ui_transaction(burn, outputs))
                .collect(),
        })
        .collect();
    let burn_bundle_bytes = storage_size
        .map(|size| size.burn_bundle_bytes)
        .unwrap_or_else(|| {
            burn_bundles
                .iter()
                .map(|bundle: &UiBurnBundle| bundle.byte_size)
                .sum::<usize>()
        });
    let total_bytes = storage_size
        .map(|size| size.total_bytes)
        .unwrap_or_else(|| {
            block
                .json_size_bytes()
                .unwrap_or_else(|_| transaction_bytes.saturating_add(burn_bundle_bytes))
        });
    UiBlock {
        height: block.height,
        prev_hash: block.prev_hash,
        timestamp_ms: block.timestamp_ms,
        miner: block.miner,
        finalizer_mode: block.finalizer_mode,
        finalizer_rank: block.finalizer_rank,
        reward: block.reward,
        total_fees,
        lost_iuna: block_lost_iuna(&block.transactions, &transactions_v2, block.reward),
        total_bytes,
        header_and_proof_bytes,
        transaction_bytes,
        transaction_byte_breakdown,
        burn_bundle_bytes,
        burn_bundle_quorum: UiBurnBundleQuorum {
            burn_bundles_included,
            committee_size: burn_bundles_required,
        },
        vdf_rounds: block.vdf_rounds,
        vdf_output: block.vdf_output,
        leader_proof: block.leader_proof,
        burn_leader_ranks: ranks,
        transactions,
        burn_bundles,
        hash: block.hash,
    }
}

fn decode_block_transactions_v2(
    block: &Block,
    expected_domain: &TransactionV2Domain,
) -> Vec<TransactionV2> {
    block
        .transactions_v2
        .iter()
        .filter_map(|envelope| {
            let bytes = decode_hex(envelope).ok()?;
            let (domain, transaction) = TransactionV2::decode(&bytes).ok()?;
            (domain == *expected_domain).then_some(transaction)
        })
        .collect()
}

fn burn_bundle_wallet_quorum(block: &Block) -> (usize, usize) {
    if block.finalizer_mode != FinalizerMode::Ticket {
        return (0, 0);
    }

    // The block records the attestations actually included in its rank-dependent
    // quorum. This UI summary does not reconstruct the historical committee from
    // unrelated burn-ticket rank owners.
    let committee_size = block.burn_bundle_section.signatures.len().saturating_add(1);
    (committee_size, committee_size)
}

fn block_lost_iuna(
    transactions: &[Transaction],
    transactions_v2: &[TransactionV2],
    reward: Amount,
) -> Amount {
    let mut burned = 0_u64;
    let mut existing_supply_fees = 0_u64;
    let mut minted_finalizer_fees = 0_u64;
    for transaction in transactions {
        match transaction {
            Transaction::Burn { amount, fee, .. } => {
                burned = burned.saturating_add(*amount);
                existing_supply_fees = existing_supply_fees.saturating_add(*fee);
            }
            Transaction::Transfer { fee, .. } => {
                existing_supply_fees = existing_supply_fees.saturating_add(*fee);
            }
            Transaction::Mine { .. } => {
                minted_finalizer_fees = minted_finalizer_fees.saturating_add(transaction.fee());
            }
        }
    }
    for transaction in transactions_v2 {
        if let TransactionV2::Burn { amount, .. } = transaction {
            burned = burned.saturating_add(*amount);
        }
        existing_supply_fees = existing_supply_fees.saturating_add(transaction.fee());
    }
    let returned_fees = reward
        .saturating_sub(minted_finalizer_fees)
        .min(existing_supply_fees);
    burned.saturating_add(existing_supply_fees.saturating_sub(returned_fees))
}

fn transaction_byte_breakdown(
    transactions: &[Transaction],
    transactions_v2: &[TransactionV2],
    storage_size: Option<&CompactBlockSizeBreakdown>,
    transaction_v2_domain: Option<&TransactionV2Domain>,
) -> Vec<UiByteBreakdown> {
    if let Some(storage_size) = storage_size {
        let mut rows = byte_breakdown_rows(
            storage_size.transfer_bytes,
            storage_size.burn_bytes,
            storage_size.mine_bytes,
        );
        let legacy_bytes = storage_size
            .transfer_bytes
            .saturating_add(storage_size.burn_bytes)
            .saturating_add(storage_size.mine_bytes);
        let v2_bytes = storage_size.transaction_bytes.saturating_sub(legacy_bytes);
        if v2_bytes > 0 {
            rows.push(UiByteBreakdown {
                label: "transaction v2",
                bytes: v2_bytes,
            });
        }
        return rows;
    }
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
    let mut rows = byte_breakdown_rows(transfer_bytes, burn_bytes, mine_bytes);
    let transaction_v2_bytes = transaction_v2_domain.map_or(0, |domain| {
        transactions_v2.iter().fold(0_usize, |total, transaction| {
            total.saturating_add(transaction.encoded_size_bytes(domain).unwrap_or_default())
        })
    });
    if transaction_v2_bytes > 0 {
        rows.push(UiByteBreakdown {
            label: "transaction v2",
            bytes: transaction_v2_bytes,
        });
    }
    rows
}

fn byte_breakdown_rows(
    transfer_bytes: usize,
    burn_bytes: usize,
    mine_bytes: usize,
) -> Vec<UiByteBreakdown> {
    [
        ("transfer", transfer_bytes),
        ("burn", burn_bytes),
        ("mine", mine_bytes),
    ]
    .into_iter()
    .filter_map(|(label, bytes)| (bytes > 0).then_some(UiByteBreakdown { label, bytes }))
    .collect()
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
        },
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
            ..
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
        },
    }
}

pub(super) fn ui_transaction_v2(
    transaction: &TransactionV2,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
) -> Result<UiTransaction> {
    let transaction_id = hex_encode(transaction.transaction_id(domain)?);
    match transaction {
        TransactionV2::Migration {
            inputs,
            outputs,
            fee,
            authorizations,
        } => Ok(UiTransaction {
            kind: "migration",
            from: inputs
                .first()
                .map(|input| encode_versioned_address(input.owner, network))
                .transpose()?
                .unwrap_or_default(),
            to: first_v2_output_address(outputs, network)?,
            amount: outputs
                .iter()
                .fold(0_u64, |total, output| total.saturating_add(output.amount)),
            fee: *fee,
            inputs: inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    let txid = match &input.outpoint_id {
                        LegacyTransactionId::Hash(value) => hex_encode(value),
                        LegacyTransactionId::Signature(value) => hex_encode(value),
                    };
                    v2_ui_input(
                        OutPoint {
                            txid,
                            index: input.outpoint_index,
                        },
                        input.owner,
                        authorizations.get(index),
                        outputs_by_outpoint,
                        network,
                    )
                })
                .collect::<Result<Vec<_>>>()?,
            outputs: v2_ui_outputs(outputs, network)?,
            change: Vec::new(),
            signature: transaction_id,
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        TransactionV2::Transfer {
            inputs,
            outputs,
            fee,
            authorizations,
        } => Ok(UiTransaction {
            kind: "transfer",
            from: inputs
                .first()
                .map(|input| encode_versioned_address(input.owner, network))
                .transpose()?
                .unwrap_or_default(),
            to: first_v2_output_address(outputs, network)?,
            amount: outputs.first().map(|output| output.amount).unwrap_or(0),
            fee: *fee,
            inputs: inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    v2_ui_input(
                        OutPoint {
                            txid: hex_encode(input.outpoint_txid),
                            index: input.outpoint_index,
                        },
                        input.owner,
                        authorizations.get(index),
                        outputs_by_outpoint,
                        network,
                    )
                })
                .collect::<Result<Vec<_>>>()?,
            outputs: v2_ui_outputs(outputs, network)?,
            change: Vec::new(),
            signature: transaction_id,
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        TransactionV2::Burn {
            inputs,
            change,
            amount,
            fee,
            authorizations,
            ..
        } => Ok(UiTransaction {
            kind: "burn",
            from: inputs
                .first()
                .map(|input| encode_versioned_address(input.owner, network))
                .transpose()?
                .unwrap_or_default(),
            to: None,
            amount: *amount,
            fee: *fee,
            inputs: inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    v2_ui_input(
                        OutPoint {
                            txid: hex_encode(input.outpoint_txid),
                            index: input.outpoint_index,
                        },
                        input.owner,
                        authorizations.get(index),
                        outputs_by_outpoint,
                        network,
                    )
                })
                .collect::<Result<Vec<_>>>()?,
            outputs: Vec::new(),
            change: v2_ui_outputs(change, network)?,
            signature: transaction_id,
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        TransactionV2::Mine {
            recipient,
            difficulty_bits,
            proof_hash,
            ..
        } => {
            let recipient = encode_versioned_address(*recipient, network)?;
            let proof_hash = hex_encode(proof_hash);
            Ok(UiTransaction {
                kind: "mine",
                from: "pow".to_string(),
                to: Some(recipient.clone()),
                amount: MINE_REWARD,
                fee: 0,
                inputs: Vec::new(),
                outputs: vec![TxOutput {
                    address: recipient,
                    amount: MINE_REWARD,
                }],
                change: Vec::new(),
                signature: transaction_id,
                difficulty_bits: Some(*difficulty_bits),
                proof_bits: Some(proof_bits(&proof_hash)),
                proof_hash: Some(proof_hash),
            })
        }
    }
}

pub(super) fn transaction_v2_input_outpoints(transaction: &TransactionV2) -> Vec<OutPoint> {
    match transaction {
        TransactionV2::Migration { inputs, .. } => inputs
            .iter()
            .map(|input| OutPoint {
                txid: match &input.outpoint_id {
                    LegacyTransactionId::Hash(value) => hex_encode(value),
                    LegacyTransactionId::Signature(value) => hex_encode(value),
                },
                index: input.outpoint_index,
            })
            .collect(),
        TransactionV2::Transfer { inputs, .. } | TransactionV2::Burn { inputs, .. } => inputs
            .iter()
            .map(|input| OutPoint {
                txid: hex_encode(input.outpoint_txid),
                index: input.outpoint_index,
            })
            .collect(),
        TransactionV2::Mine { .. } => Vec::new(),
    }
}

pub(super) fn add_pending_v2_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    pending: &[TransactionV2],
    domain: &TransactionV2Domain,
    network: AddressNetwork,
) -> Result<()> {
    for transaction in pending {
        let txid = hex_encode(transaction.transaction_id(domain)?);
        let transaction_outputs = match transaction {
            TransactionV2::Migration { outputs, .. } | TransactionV2::Transfer { outputs, .. } => {
                outputs.as_slice()
            }
            TransactionV2::Burn { change, .. } => change.as_slice(),
            TransactionV2::Mine { .. } => &[],
        };
        for (index, output) in v2_ui_outputs(transaction_outputs, network)?
            .into_iter()
            .enumerate()
        {
            outputs.insert(
                OutPoint {
                    txid: txid.clone(),
                    index: u32::try_from(index)?,
                },
                output,
            );
        }
    }
    Ok(())
}

fn first_v2_output_address(
    outputs: &[crate::domain::TransactionV2Output],
    network: AddressNetwork,
) -> Result<Option<String>> {
    outputs
        .first()
        .map(|output| encode_versioned_address(output.address, network))
        .transpose()
}

fn v2_ui_input(
    outpoint: OutPoint,
    owner: crate::domain::VersionedAddress,
    authorization: Option<&crate::domain::V2SpendingAuthorization>,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
    network: AddressNetwork,
) -> Result<UiTxInput> {
    let spent_output = outputs_by_outpoint.get(&outpoint);
    Ok(UiTxInput {
        outpoint,
        owner: encode_versioned_address(owner, network)?,
        signature: authorization
            .map(|authorization| hex_encode(authorization.signature().as_bytes()))
            .unwrap_or_default(),
        amount: spent_output.map(|output| output.amount),
        address: spent_output.map(|output| output.address.clone()),
    })
}

fn v2_ui_outputs(
    outputs: &[crate::domain::TransactionV2Output],
    network: AddressNetwork,
) -> Result<Vec<TxOutput>> {
    outputs
        .iter()
        .map(|output| {
            Ok(TxOutput {
                address: encode_versioned_address(output.address, network)?,
                amount: output.amount,
            })
        })
        .collect()
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

pub(super) fn proof_bits(hex_hash: &str) -> u32 {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::compact::CompactBlockSizeBreakdown;
    use crate::domain::{
        AddressNetwork, Amount, Block, BurnBundleSection, BurnBundleSignature, BurnLeaderRank,
        FinalizerMode, Ledger, MaskedBurn, OutPoint, Transaction, TxInput, TxOutput, Wallet,
        encode_versioned_address, hex_encode,
    };

    use super::{
        block_lost_iuna, populate_wallet_reward_flow, ui_block, ui_blocks_from_indexes,
        ui_transaction_v2, wallet_transaction_row, wallet_transaction_v2_rows,
    };
    use crate::adapters::http::types::{WalletTransactionContext, WalletTransactionFilters};

    fn burn(signature: &str) -> Transaction {
        Transaction::Burn {
            inputs: vec![TxInput {
                outpoint: OutPoint {
                    txid: format!("{signature:0<64}"),
                    index: 0,
                },
                owner: "owner".to_string(),
                signature: signature.to_string(),
            }],
            change: vec![TxOutput {
                address: "owner".to_string(),
                amount: 1,
            }],
            amount: 1,
            fee: 1,
            anchor: None,
            signature: signature.to_string(),
        }
    }

    fn transfer(signature: &str, fee: Amount) -> Transaction {
        Transaction::Transfer {
            inputs: vec![TxInput {
                outpoint: OutPoint {
                    txid: format!("{signature:0<64}"),
                    index: 0,
                },
                owner: "owner".to_string(),
                signature: signature.to_string(),
            }],
            outputs: vec![TxOutput {
                address: "recipient".to_string(),
                amount: 1,
            }],
            fee,
            signature: signature.to_string(),
        }
    }

    #[test]
    fn pending_v2_migration_is_presented_in_chain_and_wallet_views() {
        let wallet = Wallet::from_seed("pending-v2-ui-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100_000)]), 1);
        let transaction = ledger.build_v2_migration_batch(&wallet, 100).unwrap();
        let domain = ledger.transaction_v2_domain().unwrap();
        let outputs = ledger.all_utxos().into_iter().collect::<BTreeMap<_, _>>();
        let network = AddressNetwork::Mainnet;

        let chain_row = ui_transaction_v2(&transaction, &outputs, &domain, network).unwrap();
        assert_eq!(chain_row.kind, "migration");
        assert_eq!(chain_row.signature.len(), 64);
        assert_eq!(chain_row.inputs.len(), 1);
        assert_eq!(chain_row.outputs.len(), 1);

        let wallet_addresses = vec![
            wallet.address().to_string(),
            encode_versioned_address(wallet.legacy_versioned_address(), network).unwrap(),
            wallet.hybrid_address(network),
        ];
        let rows = wallet_transaction_v2_rows(
            &wallet_addresses,
            &[transaction],
            &outputs,
            WalletTransactionFilters::default(),
            &domain,
            network,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "migration");
        assert_eq!(rows[0].direction, "migrated");
        assert_eq!(rows[0].status, "pending");
        assert_eq!(rows[0].amount, 99_900);
    }

    #[test]
    fn confirmed_v2_migration_is_presented_in_block_transactions() {
        let wallet = Wallet::from_seed("confirmed-v2-chain-ui-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100_000)]), 1);
        let transaction = ledger.build_v2_migration_batch(&wallet, 100).unwrap();
        let domain = ledger.transaction_v2_domain().unwrap();
        let mut block = ledger.chain().last().unwrap().clone();
        block.transactions_v2 = vec![hex_encode(transaction.encode(&domain).unwrap())];
        let blocks = ui_blocks_from_indexes(
            vec![block],
            &ledger.all_utxos().into_iter().collect(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            Some(&domain),
            AddressNetwork::Mainnet,
        );

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].transactions.len(), 1);
        assert_eq!(blocks[0].transactions[0].kind, "migration");
        assert_eq!(blocks[0].transactions[0].fee, 100);
        assert_eq!(blocks[0].transactions[0].signature.len(), 64);
        assert_eq!(blocks[0].total_fees, 100);
    }

    #[test]
    fn wallet_reward_flow_contains_every_fee_and_payout() {
        let reward_projection = Transaction::Transfer {
            inputs: Vec::new(),
            outputs: vec![
                TxOutput {
                    address: "finalizer".to_string(),
                    amount: 3,
                },
                TxOutput {
                    address: "committee".to_string(),
                    amount: 2,
                },
            ],
            fee: 0,
            signature: "reward:block:0".to_string(),
        };
        let mut row = wallet_transaction_row(
            "finalizer",
            &reward_projection,
            &BTreeMap::new(),
            &WalletTransactionContext {
                status: "confirmed",
                block_height: Some(1),
                timestamp_ms: Some(1_000),
                block_finalizer: Some("finalizer".to_string()),
            },
        )
        .unwrap();
        row.kind = "reward";
        let committee_row = wallet_transaction_row(
            "committee",
            &reward_projection,
            &BTreeMap::new(),
            &WalletTransactionContext {
                status: "confirmed",
                block_height: Some(1),
                timestamp_ms: Some(1_000),
                block_finalizer: Some("finalizer".to_string()),
            },
        )
        .unwrap();
        assert_eq!(committee_row.amount, 2);
        assert_eq!(committee_row.direction, "received");

        let block = Block {
            height: 1,
            prev_hash: "parent".to_string(),
            timestamp_ms: 1_000,
            miner: "finalizer".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 5,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection {
                signatures: vec![BurnBundleSignature {
                    slot: 2,
                    member: "committee".to_string(),
                    signature: "attestation".to_string(),
                }],
                burns: Vec::new(),
            },
            transactions: vec![transfer("transfer", 2), burn("burn")],
            transactions_v2: Vec::new(),
            hash: "block".to_string(),
        };

        populate_wallet_reward_flow(&mut row, &block);

        assert_eq!(row.reward_fee_inputs.len(), 2);
        assert_eq!(row.reward_total, Some(5));
        assert_eq!(row.reward_fee_inputs[0].transaction_kind, "transfer");
        assert_eq!(row.reward_fee_inputs[0].amount, 2);
        assert_eq!(row.reward_fee_inputs[1].transaction_kind, "burn");
        assert_eq!(row.reward_fee_inputs[1].amount, 1);
        assert_eq!(row.reward_outputs.len(), 2);
        assert_eq!(row.reward_outputs[0].label, "Finalizer reward");
        assert_eq!(row.reward_outputs[0].address, "finalizer");
        assert_eq!(row.reward_outputs[0].amount, 3);
        assert_eq!(row.reward_outputs[1].label, "Committee reward (slot 2)");
        assert_eq!(row.reward_outputs[1].address, "committee");
        assert_eq!(row.reward_outputs[1].amount, 2);
    }

    #[test]
    fn block_lost_iuna_counts_burns_without_rewarded_fees() {
        let burn = Transaction::Burn {
            inputs: Vec::new(),
            change: Vec::new(),
            amount: 7,
            fee: 3,
            anchor: None,
            signature: "burn".to_string(),
        };

        assert_eq!(block_lost_iuna(&[burn], &[], 3), 7);
    }

    #[test]
    fn block_lost_iuna_counts_unreturned_existing_supply_fees() {
        let transfer = transfer("transfer-a", 5);

        assert_eq!(block_lost_iuna(&[transfer], &[], 2), 3);
    }

    #[test]
    fn block_lost_iuna_ignores_minted_mine_finalizer_fees() {
        let mine = Transaction::Mine {
            recipient: "miner".to_string(),
            anchor: "anchor".to_string(),
            salt: 0,
            nonce: 0,
            difficulty_bits: 1,
            proof_header: None,
            signature: "mine".to_string(),
        };

        assert_eq!(block_lost_iuna(&[mine], &[], 1), 0);
    }

    #[test]
    fn block_lost_iuna_does_not_count_mine_fees_as_returned_existing_supply() {
        let transfer = transfer("transfer-a", 5);
        let mine = Transaction::Mine {
            recipient: "miner".to_string(),
            anchor: "anchor".to_string(),
            salt: 0,
            nonce: 0,
            difficulty_bits: 1,
            proof_header: None,
            signature: "mine".to_string(),
        };
        let reward = mine.fee().saturating_add(2);

        assert_eq!(block_lost_iuna(&[transfer, mine], &[], reward), 3);
    }

    #[test]
    fn ui_block_exposes_burn_bundles() {
        let burn = burn("burn-a");
        let block = Block {
            height: 1,
            prev_hash: "parent".to_string(),
            timestamp_ms: 1,
            miner: "finalizer".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection {
                signatures: vec![crate::domain::BurnBundleSignature {
                    slot: 1,
                    member: "member-1".to_string(),
                    signature: "sig-1".to_string(),
                }],
                burns: vec![MaskedBurn {
                    burn: burn.clone(),
                    bundle_mask: 1 << 1,
                }],
            },
            transactions: vec![burn],
            transactions_v2: Vec::new(),
            hash: "hash".to_string(),
        };

        let storage_size = CompactBlockSizeBreakdown {
            total_bytes: 120,
            header_and_proof_bytes: 10,
            transaction_bytes: 80,
            transfer_bytes: 0,
            burn_bytes: 80,
            mine_bytes: 0,
            burn_bundle_bytes: 30,
        };
        let ui = ui_block(
            block,
            &BTreeMap::new(),
            &BTreeMap::new(),
            Some(&storage_size),
        );

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 2);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 2);
        assert_eq!(ui.lost_iuna, 1);
        assert_eq!(ui.burn_bundles.len(), 1);
        assert_eq!(ui.burn_bundles[0].slot, 1);
        assert_eq!(ui.burn_bundles[0].burns.len(), 1);
        assert_eq!(ui.total_bytes, 120);
        assert_eq!(ui.header_and_proof_bytes, 10);
        assert_eq!(ui.transaction_bytes, 80);
        assert_eq!(ui.burn_bundle_bytes, 30);
    }

    #[test]
    fn ui_block_counts_implicit_finalizer_attestation_in_wallet_quorum() {
        let block = Block {
            height: 1,
            prev_hash: "parent".to_string(),
            timestamp_ms: 1,
            miner: "finalizer".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: vec![burn("burn-a")],
            transactions_v2: Vec::new(),
            hash: "hash".to_string(),
        };
        let ranks = BTreeMap::from([(
            "hash".to_string(),
            vec![BurnLeaderRank {
                rank: 0,
                ticket_id: "ticket".to_string(),
                owner: "finalizer".to_string(),
                amount: 1,
                eligible_from_height: 1,
                eligible_until_height: 1,
            }],
        )]);

        let ui = ui_block(block, &BTreeMap::new(), &ranks, None);

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 1);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 1);
    }

    #[test]
    fn ui_block_burn_bundle_quorum_uses_included_lineage_attestations() {
        let block = Block {
            height: 1,
            prev_hash: "parent".to_string(),
            timestamp_ms: 1,
            miner: "finalizer".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection {
                signatures: vec![crate::domain::BurnBundleSignature {
                    slot: 1,
                    member: "member-1".to_string(),
                    signature: "sig-1".to_string(),
                }],
                burns: Vec::new(),
            },
            transactions: vec![burn("burn-a")],
            transactions_v2: Vec::new(),
            hash: "hash".to_string(),
        };
        let ranks = BTreeMap::from([(
            "hash".to_string(),
            vec![
                BurnLeaderRank {
                    rank: 0,
                    ticket_id: "ticket-0".to_string(),
                    owner: "finalizer".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 1,
                    ticket_id: "ticket-1".to_string(),
                    owner: "member-1".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 2,
                    ticket_id: "ticket-2".to_string(),
                    owner: "finalizer".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 3,
                    ticket_id: "ticket-3".to_string(),
                    owner: "member-1".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 4,
                    ticket_id: "ticket-4".to_string(),
                    owner: "finalizer".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
            ],
        )]);

        let ui = ui_block(block, &BTreeMap::new(), &ranks, None);

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 2);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 2);
    }

    #[test]
    fn ui_block_burn_bundle_quorum_ignores_unrelated_ticket_rank_window() {
        let block = Block {
            height: 1,
            prev_hash: "parent".to_string(),
            timestamp_ms: 1,
            miner: "fallback".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 1,
            reward: 1,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection {
                signatures: vec![crate::domain::BurnBundleSignature {
                    slot: 1,
                    member: "member-1".to_string(),
                    signature: "sig-1".to_string(),
                }],
                burns: Vec::new(),
            },
            transactions: vec![burn("burn-a")],
            transactions_v2: Vec::new(),
            hash: "hash".to_string(),
        };
        let ranks = BTreeMap::from([(
            "hash".to_string(),
            vec![
                BurnLeaderRank {
                    rank: 0,
                    ticket_id: "ticket-0".to_string(),
                    owner: "missed-primary".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 1,
                    ticket_id: "ticket-1".to_string(),
                    owner: "fallback".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 2,
                    ticket_id: "ticket-2".to_string(),
                    owner: "member-1".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
                BurnLeaderRank {
                    rank: 3,
                    ticket_id: "ticket-3".to_string(),
                    owner: "member-2".to_string(),
                    amount: 1,
                    eligible_from_height: 1,
                    eligible_until_height: 1,
                },
            ],
        )]);

        let ui = ui_block(block, &BTreeMap::new(), &ranks, None);

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 2);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 2);
    }
}
