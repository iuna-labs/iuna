use std::collections::{BTreeMap, BTreeSet};

use super::{
    AddressNetwork, Ledger, OutPoint, TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT,
    TransactionV2, TxOutput, UtxoLineageRoot, VersionedAddress, Wallet,
};
use crate::app::NodeCore;

fn fixture(count: usize, value: u64, roots: bool) -> (Wallet, Ledger) {
    let wallet = Wallet::from_seed("consolidation-tests");
    let mut ledger = Ledger::new(BTreeMap::new(), 1);
    for index in 0..count {
        let point = OutPoint {
            txid: format!("{:064x}", index + 1),
            index: 0,
        };
        ledger.utxos.insert(
            point.clone(),
            TxOutput {
                address: wallet.address().to_string(),
                amount: value,
            },
        );
        if roots {
            ledger.utxo_lineage.insert(
                point.clone(),
                UtxoLineageRoot {
                    outpoint: point,
                    height: 0,
                },
            );
        }
    }
    (wallet, ledger)
}

fn hybrid_inputs(wallet: &Wallet, points: &[OutPoint]) -> Vec<(OutPoint, VersionedAddress)> {
    points
        .iter()
        .cloned()
        .map(|point| (point, wallet.hybrid_versioned_address()))
        .collect()
}

#[test]
fn consolidation_large_wallet_is_bounded_disjoint_and_keeps_a_reserve() {
    let (wallet, ledger) = fixture(1247, 1_000_000, false);
    let mut node = NodeCore::from_ledger(wallet.clone(), ledger, 0);
    let plan = node.consolidation_plan(1, false).unwrap();
    assert_eq!(plan.before, 1247);
    assert_eq!(plan.batches.len(), 10);
    assert_eq!(plan.after, 11);
    let mut selected = BTreeSet::new();
    for batch in &plan.batches {
        assert!((2..=128).contains(&batch.utxos.len()));
        for point in &batch.utxos {
            assert!(selected.insert(point.clone()));
        }
        assert!(batch.fee >= batch.bytes as u64);
        let tx = node
            .consolidate(&batch.utxos, 1, batch.fee, false, wallet.address())
            .unwrap();
        assert!(!tx.is_empty());
        let pending = node
            .pending_transactions()
            .into_iter()
            .find(|transaction| transaction.signature() == tx)
            .unwrap();
        let outputs = pending.outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].address, wallet.address());
        assert_eq!(
            outputs[0].amount + pending.fee(),
            batch.utxos.len() as u64 * 1_000_000
        );
    }
    assert_eq!(selected.len(), 1246);
    assert_eq!(
        plan.fee,
        plan.batches.iter().map(|batch| batch.fee).sum::<u64>()
    );
    let batch = &plan.batches[0];
    assert!(
        node.consolidate(&batch.utxos, 1, batch.fee, false, wallet.address())
            .is_err()
    );
}

#[test]
fn consolidation_plan_includes_hybrid_outputs_in_separate_v2_batches() {
    let (wallet, mut ledger) = fixture(3, 1_000_000, false);
    let hybrid_address = wallet.hybrid_address(AddressNetwork::Mainnet);
    for index in 0..3 {
        ledger.utxos.insert(
            OutPoint {
                txid: format!("{:064x}", index + 10),
                index: 0,
            },
            TxOutput {
                address: hybrid_address.clone(),
                amount: 2_000_000,
            },
        );
    }

    let node = NodeCore::from_ledger(wallet, ledger, 0);
    let plan = node.consolidation_plan(1, true).unwrap();
    assert_eq!(plan.before, 6);
    assert_eq!(plan.after, 4);
    assert_eq!(plan.batches.len(), 2);
    assert_eq!(
        plan.batches
            .iter()
            .filter(|batch| batch.kind == crate::app::ConsolidationKind::Legacy)
            .count(),
        1
    );
    assert_eq!(
        plan.batches
            .iter()
            .filter(|batch| batch.kind == crate::app::ConsolidationKind::Hybrid)
            .count(),
        1
    );
}

#[test]
fn selected_hybrid_outputs_build_one_v2_consolidation_output() {
    let wallet = Wallet::from_seed("hybrid-consolidation-builder");
    let address = wallet.hybrid_address(AddressNetwork::Mainnet);
    let points = (0..2)
        .map(|index| OutPoint {
            txid: format!("{:064x}", index + 1),
            index: 0,
        })
        .collect::<Vec<_>>();
    let mut ledger = Ledger::new(BTreeMap::new(), 1);
    for point in &points {
        ledger.utxos.insert(
            point.clone(),
            TxOutput {
                address: address.clone(),
                amount: 1_000_000,
            },
        );
    }

    let transaction = ledger
        .build_v2_consolidation_with_input_owners(
            &wallet,
            1_990_000,
            10_000,
            &hybrid_inputs(&wallet, &points),
        )
        .unwrap();
    let TransactionV2::Transfer {
        inputs,
        outputs,
        fee,
        ..
    } = transaction
    else {
        panic!("expected a transaction-v2 transfer");
    };
    assert_eq!(inputs.len(), 2);
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].address, wallet.hybrid_versioned_address());
    assert_eq!(outputs[0].amount, 1_990_000);
    assert_eq!(fee, 10_000);
}

#[test]
fn hybrid_consolidation_preview_matches_signed_size_before_and_after_aggregation() {
    let activation = TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT;
    for next_height in [activation - 1, activation] {
        let wallet = Wallet::from_seed(&format!("hybrid-preview-{next_height}"));
        let address = wallet.hybrid_address(AddressNetwork::Mainnet);
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.chain.last_mut().unwrap().height = next_height - 1;
        for index in 0..4 {
            ledger.utxos.insert(
                OutPoint {
                    txid: format!("{:064x}", index + 1),
                    index: 0,
                },
                TxOutput {
                    address: address.clone(),
                    amount: 2_000_000,
                },
            );
        }

        let node = NodeCore::from_ledger(wallet.clone(), ledger.clone(), 0);
        let plan = node.consolidation_plan(1, true).unwrap();
        let batch = plan
            .batches
            .iter()
            .find(|batch| batch.kind == crate::app::ConsolidationKind::Hybrid)
            .unwrap();
        let transaction = ledger
            .build_v2_consolidation_with_input_owners(
                &wallet,
                batch.amount,
                batch.fee,
                &hybrid_inputs(&wallet, &batch.utxos),
            )
            .unwrap();
        let TransactionV2::Transfer { authorizations, .. } = &transaction else {
            panic!("expected a transaction-v2 transfer");
        };
        let expected_authorizations = if next_height < activation {
            batch.utxos.len()
        } else {
            1
        };

        assert_eq!(authorizations.len(), expected_authorizations);
        assert_eq!(
            batch.bytes,
            transaction
                .encoded_size_bytes(&ledger.transaction_v2_domain().unwrap())
                .unwrap()
        );
        assert_eq!(batch.fee, batch.bytes as u64);
    }
}

#[test]
fn hybrid_consolidation_preview_scales_to_a_large_pre_aggregation_wallet() {
    let wallet = Wallet::from_seed("large-hybrid-preview");
    let address = wallet.hybrid_address(AddressNetwork::Mainnet);
    let mut ledger = Ledger::new(BTreeMap::new(), 1);
    ledger.chain.last_mut().unwrap().height =
        TRANSACTION_V2_AUTHORIZATION_AGGREGATION_ACTIVATION_HEIGHT - 300;
    for index in 0..3_800 {
        ledger.utxos.insert(
            OutPoint {
                txid: format!("{:064x}", index + 1),
                index: 0,
            },
            TxOutput {
                address: address.clone(),
                amount: 2_000_000,
            },
        );
    }

    let node = NodeCore::from_ledger(wallet, ledger, 0);
    let plan = node.consolidation_plan(1, true).unwrap();

    assert_eq!(plan.before, 3_800);
    assert_eq!(plan.batches.len(), 30);
    assert!(
        plan.batches
            .iter()
            .all(|batch| batch.kind == crate::app::ConsolidationKind::Hybrid)
    );
    assert_eq!(plan.batches[0].utxos.len(), 128);
}

#[test]
fn consolidation_plan_excludes_outputs_reserved_by_pending_v2() {
    let wallet = Wallet::from_seed("pending-v2-consolidation");
    let address = wallet.hybrid_address(AddressNetwork::Mainnet);
    let points = (0..3)
        .map(|index| OutPoint {
            txid: format!("{:064x}", index + 1),
            index: 0,
        })
        .collect::<Vec<_>>();
    let mut ledger = Ledger::new(BTreeMap::new(), 1);
    for point in &points {
        ledger.utxos.insert(
            point.clone(),
            TxOutput {
                address: address.clone(),
                amount: 1_000_000,
            },
        );
    }
    let pending = ledger
        .build_v2_consolidation_with_input_owners(
            &wallet,
            1_990_000,
            10_000,
            &hybrid_inputs(&wallet, &points[..2]),
        )
        .unwrap();
    ledger.pending_v2.push(pending);

    let node = NodeCore::from_ledger(wallet, ledger, 0);
    let plan = node.consolidation_plan(1, true).unwrap();
    assert_eq!(plan.before, 3);
    assert_eq!(plan.after, 3);
    assert!(plan.batches.is_empty());
}

#[test]
fn consolidation_requires_consent_for_distinct_mining_roots_and_respects_fee_cap() {
    let (wallet, ledger) = fixture(4, 1_000_000, true);
    let mut node = NodeCore::from_ledger(wallet.clone(), ledger, 0);
    assert!(
        node.consolidation_plan(1, false)
            .unwrap()
            .batches
            .is_empty()
    );
    let plan = node.consolidation_plan(1, true).unwrap();
    let batch = &plan.batches[0];
    assert!(
        node.consolidate(&batch.utxos, 1, batch.fee, false, wallet.address())
            .is_err()
    );
    assert!(
        node.consolidate(&batch.utxos, 1, batch.fee - 1, true, wallet.address())
            .is_err()
    );
    assert!(
        node.consolidate(&batch.utxos, 1, batch.fee, true, "another-wallet")
            .is_err()
    );
    assert!(
        node.consolidate(
            &[batch.utxos[0].clone(), batch.utxos[0].clone()],
            1,
            batch.fee,
            true,
            wallet.address()
        )
        .is_err()
    );
}

#[test]
fn consolidation_skips_uneconomic_and_locked_wallets() {
    let (wallet, ledger) = fixture(3, 10, false);
    let node = NodeCore::from_ledger(wallet.clone(), ledger.clone(), 0);
    assert!(
        node.consolidation_plan(1, false)
            .unwrap()
            .batches
            .is_empty()
    );
    let locked = NodeCore::from_locked_wallet_address(wallet.address(), ledger, false, 0, 1);
    assert!(locked.consolidation_plan(1, false).is_err());
}

#[test]
fn consolidation_large_uneconomic_wallet_skips_dust_without_building_transactions() {
    let (wallet, ledger) = fixture(2000, 10, false);
    let node = NodeCore::from_ledger(wallet, ledger, 0);
    let plan = node.consolidation_plan(1, false).unwrap();
    assert_eq!(plan.before, 2000);
    assert_eq!(plan.after, 2000);
    assert!(plan.batches.is_empty());
}

#[test]
fn consolidation_adapts_to_the_network_block_limit() {
    let (wallet, mut ledger) = fixture(150, 1_000_000, false);
    ledger.launch_profile.max_block_bytes = 3000;
    let node = NodeCore::from_ledger(wallet, ledger, 0);
    let plan = node.consolidation_plan(1, false).unwrap();
    assert!(!plan.batches.is_empty());
    assert!(plan.batches.iter().all(|batch| batch.utxos.len() < 128));
}

#[test]
fn transfer_selection_uses_best_fit_then_largest_inputs() {
    let (wallet, mut ledger) = fixture(4, 10, false);
    ledger
        .utxos
        .values_mut()
        .zip([10, 100, 1000, 200])
        .for_each(|(output, amount)| output.amount = amount);
    let single = ledger
        .build_transfer(&wallet, wallet.address(), 99, 1)
        .unwrap();
    assert_eq!(single.inputs().len(), 1);
    assert_eq!(ledger.utxos[&single.inputs()[0].outpoint].amount, 100);
    let multiple = ledger
        .build_transfer(&wallet, wallet.address(), 1100, 1)
        .unwrap();
    assert_eq!(multiple.inputs().len(), 2);
    assert_eq!(ledger.utxos[&multiple.inputs()[0].outpoint].amount, 1000);
    assert_eq!(ledger.utxos[&multiple.inputs()[1].outpoint].amount, 200);
}
