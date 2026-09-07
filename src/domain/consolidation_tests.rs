use std::collections::{BTreeMap, BTreeSet};

use super::{Ledger, OutPoint, TxOutput, UtxoLineageRoot, Wallet};
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
        let outputs = tx.outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].address, wallet.address());
        assert_eq!(
            outputs[0].amount + tx.fee(),
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
