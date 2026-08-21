use std::collections::BTreeMap;

use crate::domain::{
    Amount, Block, BurnLeaderRank, ChainSnapshot, FinalizerMode, MINE_REWARD, OutPoint,
    Transaction, TxInput, TxOutput,
};

use crate::adapters::ui_index::build_ui_chain_index;

use super::{
    HttpState, UiChainView,
    types::{
        UiBlock, UiBurnBundle, UiBurnBundleQuorum, UiByteBreakdown, UiTransaction, UiTxInput,
        WalletTransactionContext, WalletTransactionFilters, WalletTransactionRow,
    },
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
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
        }),
        _ => None,
    }
}

pub(super) fn ui_blocks_from_indexes(
    blocks: Vec<Block>,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
) -> Vec<UiBlock> {
    blocks
        .into_iter()
        .map(|block| ui_block(block, outputs, burn_leader_ranks))
        .collect()
}

pub(super) fn ui_block(
    block: Block,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
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
    let total_fees = public_fees;
    let transaction_bytes = block
        .transactions
        .iter()
        .map(|tx| tx.serialized_size_bytes().unwrap_or_default())
        .sum::<usize>();
    let transaction_byte_breakdown = transaction_byte_breakdown(&block.transactions);
    let transactions = block
        .transactions
        .iter()
        .map(|tx| ui_transaction(tx, outputs))
        .collect::<Vec<_>>();
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
    let burn_bundle_bytes = burn_bundles
        .iter()
        .map(|bundle: &UiBurnBundle| bundle.byte_size)
        .sum::<usize>();
    let total_bytes = block
        .serialized_size_bytes()
        .unwrap_or_else(|_| transaction_bytes.saturating_add(burn_bundle_bytes));
    UiBlock {
        height: block.height,
        prev_hash: block.prev_hash,
        timestamp_ms: block.timestamp_ms,
        miner: block.miner,
        finalizer_mode: block.finalizer_mode,
        finalizer_rank: block.finalizer_rank,
        reward: block.reward,
        total_fees,
        lost_iuna: block_lost_iuna(&block.transactions, block.reward),
        total_bytes,
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

fn burn_bundle_wallet_quorum(block: &Block) -> (usize, usize) {
    if block.finalizer_mode != FinalizerMode::Ticket {
        return (0, 0);
    }

    // Consensus requires every available explicit committee signature for rank 0
    // and rank 1. Rank 2 and later have no additional committee slots. Therefore
    // an accepted ticket block records its actual quorum without consulting the
    // unrelated burn-ticket rank owners.
    let committee_size = block.burn_bundle_section.signatures.len().saturating_add(1);
    (committee_size, committee_size)
}

fn block_lost_iuna(transactions: &[Transaction], reward: Amount) -> Amount {
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
    let returned_fees = reward
        .saturating_sub(minted_finalizer_fees)
        .min(existing_supply_fees);
    burned.saturating_add(existing_supply_fees.saturating_sub(returned_fees))
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
    cache.burn_leader_ranks_by_hash = view.burn_leader_ranks_by_hash.clone();
    Ok(UiChainView {
        outputs: view.outputs,
        burn_leader_ranks_by_hash: view.burn_leader_ranks_by_hash,
    })
}

pub(super) async fn cached_ui_blocks_for_tip(
    state: &HttpState,
    tip_hash: Option<&str>,
    blocks: Vec<Block>,
) -> Option<Vec<UiBlock>> {
    let cache = state.ui_cache.lock().await;
    (cache.tip_hash.as_deref() == tip_hash)
        .then(|| ui_blocks_from_indexes(blocks, &cache.outputs, &cache.burn_leader_ranks_by_hash))
}

fn ui_chain_view_from_cache(cache: &super::UiChainCache) -> UiChainView {
    UiChainView {
        outputs: cache.outputs.clone(),
        burn_leader_ranks_by_hash: cache.burn_leader_ranks_by_hash.clone(),
    }
}

fn build_chain_view(snapshot: &ChainSnapshot) -> (Option<String>, UiChainView) {
    let index = build_ui_chain_index(snapshot);
    (
        index.tip_hash.clone(),
        UiChainView {
            outputs: index.outputs,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::{
        Amount, Block, BurnBundleSection, BurnLeaderRank, FinalizerMode, MaskedBurn, OutPoint,
        Transaction, TxInput, TxOutput,
    };

    use super::{block_lost_iuna, ui_block};

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
    fn block_lost_iuna_counts_burns_without_rewarded_fees() {
        let burn = Transaction::Burn {
            inputs: Vec::new(),
            change: Vec::new(),
            amount: 7,
            fee: 3,
            signature: "burn".to_string(),
        };

        assert_eq!(block_lost_iuna(&[burn], 3), 7);
    }

    #[test]
    fn block_lost_iuna_counts_unreturned_existing_supply_fees() {
        let transfer = transfer("transfer-a", 5);

        assert_eq!(block_lost_iuna(&[transfer], 2), 3);
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

        assert_eq!(block_lost_iuna(&[mine], 1), 0);
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

        assert_eq!(block_lost_iuna(&[transfer, mine], reward), 3);
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
            hash: "hash".to_string(),
        };

        let ui = ui_block(block, &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 2);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 2);
        assert_eq!(ui.lost_iuna, 1);
        assert_eq!(ui.burn_bundles.len(), 1);
        assert_eq!(ui.burn_bundles[0].slot, 1);
        assert_eq!(ui.burn_bundles[0].burns.len(), 1);
        assert!(ui.burn_bundle_bytes > 0);
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

        let ui = ui_block(block, &BTreeMap::new(), &ranks);

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

        let ui = ui_block(block, &BTreeMap::new(), &ranks);

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

        let ui = ui_block(block, &BTreeMap::new(), &ranks);

        assert_eq!(ui.burn_bundle_quorum.burn_bundles_included, 2);
        assert_eq!(ui.burn_bundle_quorum.committee_size, 2);
    }
}
