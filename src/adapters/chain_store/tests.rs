use std::collections::BTreeMap;

use tempfile::tempdir;

use crate::domain::{GenesisBurn, Ledger, Wallet, run_vdf};

use super::{SqliteChainStore, decode_compact_snapshot, encode_compact_snapshot};

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
fn sqlite_chain_store_does_not_create_ui_projection_tables() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();

    store
        .with_connection(|connection| {
            for table in [
                "block_metrics",
                "ui_cache_meta",
                "ui_output_index",
                "ui_revealed_transactions",
                "ui_burn_leader_ranks",
                "ui_burn_leader_rank_blocks",
            ] {
                let count = connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, u64>(0),
                )?;
                assert_eq!(count, 0, "{table} should live in ui_data.sqlite3");
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn sqlite_chain_store_clear_chain_removes_snapshot() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("clear-chain-alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();

    store.save(&ledger.snapshot()).unwrap();
    assert!(store.load().unwrap().is_some());

    store.clear_chain().unwrap();

    assert!(store.load().unwrap().is_none());
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
