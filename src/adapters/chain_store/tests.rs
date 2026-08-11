use std::collections::BTreeMap;

use rusqlite::Connection;
use tempfile::tempdir;

use crate::domain::{BLOCK_REWARD, GenesisBurn, Ledger, Wallet, run_vdf};

use super::{
    BlockMetricRow, SqliteChainStore, decode_compact_snapshot, encode_compact_snapshot,
    replace_metrics,
};

#[test]
fn sqlite_chain_store_roundtrips_snapshot() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("nested/chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();

    store.save(&ledger.snapshot()).unwrap();

    assert_eq!(store.load().unwrap(), Some(ledger.snapshot()));
    store
        .with_connection(|connection| {
            let columns = connection
                .prepare("PRAGMA table_info(chain_snapshots)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert!(columns.contains(&"snapshot_blob".to_string()));
            assert!(!columns.contains(&"snapshot_json".to_string()));
            Ok(())
        })
        .unwrap();
}

#[test]
fn compact_snapshot_roundtrips_and_is_smaller_than_json() {
    let alice = Wallet::from_seed("compact-alice");
    let bob = Wallet::from_seed("compact-bob");
    let carol = Wallet::from_seed("compact-carol");
    let wallets = [alice.clone(), bob.clone()];
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 10_000_000);
    genesis.insert(bob.address().to_string(), 10_000_000);
    genesis.insert(carol.address().to_string(), 10_000_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![
            GenesisBurn::new(alice.address(), 1_000_000),
            GenesisBurn::new(bob.address(), 1_000_000),
        ],
        1,
    )
    .unwrap();
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallets
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap();
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(wallet, 1).unwrap();
    assert_eq!(
        block.blinded_transactions,
        vec![blinded.transaction.clone()]
    );
    ledger.apply_locally_mined_block(block).unwrap();
    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallets
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap();
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallets
                .iter()
                .find(|wallet| wallet.address() == member.owner)
                .unwrap();
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(!bundles.is_empty());
    let prepared = ledger
        .prepare_next_block_with_reveal_bundles(wallet.address(), 2, bundles)
        .unwrap();
    let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
    let block = prepared.finish(wallet, vdf_output);
    assert_eq!(block.all_blinded_reveals().len(), 1);
    ledger.apply_locally_mined_block(block).unwrap();

    let snapshot = ledger.snapshot();
    let compact = encode_compact_snapshot(&snapshot).unwrap();
    let json = serde_json::to_vec(&snapshot).unwrap();

    assert_eq!(decode_compact_snapshot(&compact).unwrap(), snapshot);
    assert!(
        compact.len() < json.len(),
        "compact snapshot should be smaller than JSON: compact={} JSON={}",
        compact.len(),
        json.len()
    );
}

#[test]
fn sqlite_chain_store_overwrites_latest_snapshot() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 2);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();
    store.save(&ledger.snapshot()).unwrap();

    let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();
    store.save(&ledger.snapshot()).unwrap();

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.blocks.last().unwrap().height, 1);
    assert_eq!(restored, ledger.snapshot());
}

#[test]
fn sqlite_chain_store_saves_and_clears_block_metrics() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("metrics-alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();
    let burn = ledger.build_burn(&wallet, 2, 1).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    let metrics = store.load_metrics().unwrap();

    assert_eq!(metrics.last().unwrap().height, 1);
    assert_eq!(metrics.last().unwrap().burn_count, 1);
    assert_eq!(metrics.last().unwrap().burned_amount, 2);
    assert_eq!(metrics.last().unwrap().fees_amount, 1);
    assert_eq!(
        metrics.last().unwrap().circulating_supply,
        ledger.status().balances.values().copied().sum::<u64>()
    );
    assert_eq!(metrics.last().unwrap().known_wallet_addresses, 1);

    store.clear_metrics().unwrap();
    assert!(store.load_metrics().unwrap().is_empty());
}

#[test]
fn sqlite_chain_store_loads_recent_metrics_in_height_order() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("recent-metrics-alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();

    for timestamp_ms in [1_000, 2_000, 3_000] {
        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&wallet, timestamp_ms).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
    }
    store.save_with_metrics(&ledger.snapshot(), true).unwrap();

    let metrics = store.load_recent_metrics(2).unwrap();

    assert_eq!(
        metrics.iter().map(|row| row.height).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn sqlite_chain_store_migrates_known_wallet_address_metrics_column() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chain.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            r#"
CREATE TABLE block_metrics (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_time_ms INTEGER,
    mine_difficulty_bits INTEGER NOT NULL,
    circulating_supply INTEGER NOT NULL,
    transaction_count INTEGER NOT NULL,
    transfer_count INTEGER NOT NULL,
    burn_count INTEGER NOT NULL,
    mine_count INTEGER NOT NULL,
    burned_amount INTEGER NOT NULL,
    total_burned_amount INTEGER NOT NULL,
    fees_amount INTEGER NOT NULL,
    reward_amount INTEGER NOT NULL,
    vdf_rounds INTEGER NOT NULL,
    finalizer_rank INTEGER NOT NULL
);
"#,
        )
        .unwrap();
    drop(connection);

    let store = SqliteChainStore::open(&path).unwrap();

    store
            .with_connection(|connection| {
                let count = connection
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info('block_metrics') WHERE name = 'known_wallet_addresses'",
                        [],
                        |row| row.get::<_, u64>(0),
                    )
                    .unwrap();
                assert_eq!(count, 1);
                Ok(())
            })
            .unwrap();
}

#[test]
fn sqlite_chain_store_metrics_supply_matches_wallet_balances_after_block_rewards() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let alice = Wallet::from_seed("metrics-supply-alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 100);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(alice.address(), 1)], 1)
            .unwrap();

    for timestamp_ms in [1_000, 2_000, 3_000] {
        let burn = ledger.build_burn(&alice, 1, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&alice, timestamp_ms).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
    }

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    let metrics = store.load_metrics().unwrap();
    let supply_from_balances = ledger.status().balances.values().copied().sum::<u64>();

    assert_eq!(metrics.last().unwrap().height, 3);
    assert_eq!(
        metrics.last().unwrap().circulating_supply,
        supply_from_balances
    );
}

#[test]
fn sqlite_chain_store_metrics_count_known_wallet_addresses_seen_on_chain() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let alice = Wallet::from_seed("metrics-address-alice");
    let bob = Wallet::from_seed("metrics-address-bob");
    let carol = Wallet::from_seed("metrics-address-carol");
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 100);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(alice.address(), 1)], 1)
            .unwrap();

    let burn = ledger.build_burn(&alice, 1, 1).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let transfer = ledger.build_transfer(&alice, bob.address(), 10, 1).unwrap();
    ledger.submit_transaction(transfer).unwrap();
    let mine = ledger.build_mine(carol.address()).unwrap();
    ledger.submit_transaction(mine).unwrap();
    let block = ledger.mine_next_block(&alice, 1_000).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    let metrics = store.load_metrics().unwrap();

    assert_eq!(metrics[0].known_wallet_addresses, 1);
    assert_eq!(metrics.last().unwrap().known_wallet_addresses, 3);
}

#[test]
fn sqlite_chain_store_metrics_include_revealed_blinded_burns() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let alice = Wallet::from_seed("metrics-blinded-alice");
    let bob = Wallet::from_seed("metrics-blinded-bob");
    let carol = Wallet::from_seed("metrics-blinded-carol");
    let wallets = [alice.clone(), bob.clone()];
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 10_000_000);
    genesis.insert(bob.address().to_string(), 10_000_000);
    genesis.insert(carol.address().to_string(), 10_000_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![
            GenesisBurn::new(alice.address(), 1_000_000),
            GenesisBurn::new(bob.address(), 1_000_000),
        ],
        1,
    )
    .unwrap();
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallets
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap();
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(wallet, 1).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();
    let supply_after_commit = ledger.status().balances.values().copied().sum::<u64>();
    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallets
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap();
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallets
                .iter()
                .find(|wallet| wallet.address() == member.owner)
                .unwrap();
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(!bundles.is_empty());
    let prepared = ledger
        .prepare_next_block_with_reveal_bundles(wallet.address(), 2, bundles)
        .unwrap();
    let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
    let block = prepared.finish(wallet, vdf_output);
    assert_eq!(block.all_blinded_reveals().len(), 1);
    ledger.apply_locally_mined_block(block).unwrap();

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    let metrics = store.load_metrics().unwrap();
    let commit = metrics
        .iter()
        .find(|metric| metric.height == 1)
        .expect("commit block metrics should exist");
    let last = metrics.last().unwrap();
    let supply_from_balances = ledger.status().balances.values().copied().sum::<u64>();

    assert_eq!(commit.circulating_supply, supply_after_commit + 10_000_000);
    assert_eq!(last.burn_count, 2);
    assert_eq!(last.burned_amount, 4);
    assert_eq!(last.fees_amount, 7);
    assert_eq!(last.circulating_supply, supply_from_balances);
    assert_eq!(last.known_wallet_addresses, 3);
}

#[test]
fn sqlite_chain_store_roundtrips_vdf_round_metrics_above_legacy_u32_limit() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let vdf_rounds = u64::from(u32::MAX) + 42;

    store
        .with_connection_mut(|connection| {
            let transaction = connection.transaction().unwrap();
            replace_metrics(
                &transaction,
                &[BlockMetricRow {
                    height: 1,
                    block_hash: "hash".to_string(),
                    timestamp_ms: 1_000,
                    block_time_ms: Some(415_000),
                    mine_difficulty_bits: 12,
                    circulating_supply: 100,
                    known_wallet_addresses: 1,
                    transaction_count: 0,
                    transfer_count: 0,
                    burn_count: 0,
                    mine_count: 0,
                    burned_amount: 0,
                    total_burned_amount: 0,
                    fees_amount: 0,
                    reward_amount: 0,
                    vdf_rounds,
                    finalizer_rank: 0,
                }],
            )?;
            transaction.commit().unwrap();
            Ok(())
        })
        .unwrap();

    let metrics = store.load_metrics().unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].vdf_rounds, vdf_rounds);
}

#[test]
fn sqlite_chain_store_metrics_include_genesis_reward_when_burn_consumes_allocation() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("metrics-genesis-reward");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    let metrics = store.load_metrics().unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].height, 0);
    assert_eq!(metrics[0].burned_amount, 1);
    assert_eq!(metrics[0].reward_amount, BLOCK_REWARD);
    assert_eq!(metrics[0].circulating_supply, BLOCK_REWARD);
}

#[test]
fn sqlite_chain_store_disabled_metrics_save_deletes_old_metrics() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("metrics-cleanup");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();

    store.save_with_metrics(&ledger.snapshot(), true).unwrap();
    assert!(!store.load_metrics().unwrap().is_empty());

    store.save_with_metrics(&ledger.snapshot(), false).unwrap();
    assert!(store.load_metrics().unwrap().is_empty());
}

#[test]
fn sqlite_chain_store_reports_invalid_compact_snapshot() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    store
        .with_connection(|connection| {
            connection.execute(
                r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, 9, 'bad-tip', x'00010203', 0)
"#,
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let error = store.load().unwrap_err();

    assert!(
        format!("{error:#}").contains("failed to parse compact chain snapshot from database"),
        "{error:#}"
    );
}
