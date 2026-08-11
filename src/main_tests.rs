use std::{collections::BTreeMap, sync::Arc, time::Duration};

use iuna::{
    adapters::{chain_store::SqliteChainStore, config_store::UiConfig, wallet_store},
    app::{DEFAULT_BURN_PER_BLOCK, NodeCore},
    domain::{BLOCK_REWARD, GenesisBurn, Ledger, MICRO_IUNA, VDF_TARGET_BLOCK_MS, Wallet},
};
use rusqlite::Connection;
use tempfile::tempdir;
use tokio::sync::Mutex;

use super::{
    ChainMode, CliOptions, GENESIS_INITIAL_BURN_FEE, GENESIS_INITIAL_BURN_PER_BLOCK, StartupWallet,
    apply_cli_p2p_config_overrides, configured_p2p_announce_addr, configured_p2p_bind_addr,
    extrapolate_vdf_rounds, help_text, initial_burn_fee, initial_burn_per_block, initialize_ledger,
    load_startup_wallet, measure_vdf_rounds, persist_chain_snapshot,
    run_chain_persistence_with_interval, validate_wallet_for_mode,
};

fn parse(args: &[&str]) -> anyhow::Result<Option<CliOptions>> {
    CliOptions::parse_from(args.iter().map(|arg| arg.to_string()))
}

#[test]
fn help_mentions_dev_seed_verify_bypass_env() {
    assert!(help_text().contains("IUNA_DEV_SKIP_SEED_VERIFY=1"));
    assert!(help_text().contains("skip seed verification"));
    assert!(help_text().contains("--stratum <addr:port>"));
    assert!(help_text().contains("--debug"));
}

fn ledger_with_one_spendable_iuna(wallet: &Wallet) -> Ledger {
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 2);
    Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1).unwrap()
}

fn ledger_with_one_mined_block(wallet: &Wallet) -> Ledger {
    let mut ledger = ledger_with_one_spendable_iuna(wallet);
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(wallet, 1_000).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();
    ledger
}

#[test]
fn encrypted_startup_wallet_loads_as_locked_metadata() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("wallet.json");
    let (wallet, _) =
        wallet_store::replace_with_generated_seed_phrase_encrypted(&path, "password-123456")
            .unwrap();

    let startup = load_startup_wallet(&path).unwrap();

    match startup {
        StartupWallet::Locked { address } => assert_eq!(address, wallet.address()),
        StartupWallet::Unlocked { .. } => panic!("encrypted wallet should start locked"),
    }
}

#[test]
fn no_args_starts_setup_mode() {
    let opts = parse(&[]).unwrap().unwrap();
    assert_eq!(opts.chain_mode, ChainMode::Setup);
    assert!(opts.join_peers.is_empty());
}

#[test]
fn stratum_port_can_be_configured() {
    let opts = parse(&["--stratum", "127.0.0.1:3333"]).unwrap().unwrap();
    assert_eq!(opts.stratum_addr, Some("127.0.0.1:3333".parse().unwrap()));
}

#[test]
fn debug_logging_can_be_enabled() {
    assert!(!parse(&[]).unwrap().unwrap().debug);
    assert!(parse(&["--debug"]).unwrap().unwrap().debug);
}

#[test]
fn removed_wallet_seed_is_rejected() {
    let error = parse(&["--wallet-seed", "alice", "--genesis"]).unwrap_err();
    assert!(error.to_string().contains("--wallet-seed was removed"));
}

#[test]
fn runtime_configuration_flags_are_rejected() {
    for flag in [
        "--start",
        "--name",
        "--burn-per-block",
        "--peer",
        "--genesis-amount",
        "--vdf-rounds",
    ] {
        let error = parse(&["--genesis", flag, "value"]).unwrap_err();
        assert!(
            error.to_string().contains("unknown argument"),
            "{flag} should not be accepted"
        );
    }
}

#[test]
fn genesis_mode_is_explicit() {
    let opts = parse(&["--genesis"]).unwrap().unwrap();
    assert_eq!(opts.chain_mode, ChainMode::Genesis);
    assert!(opts.join_peers.is_empty());
}

#[test]
fn join_mode_does_not_start_new_chain() {
    let opts = parse(&["--join", "127.0.0.1:9444"]).unwrap().unwrap();
    assert_eq!(opts.chain_mode, ChainMode::Join);
    assert_eq!(opts.join_peers, vec!["127.0.0.1:9444"]);
}

#[test]
fn genesis_mode_starts_with_default_burn_rate_and_fee() {
    let genesis = parse(&["--genesis"]).unwrap().unwrap();
    let configured = UiConfig {
        burn_per_block: 50,
        burn_fee: 3,
        ..UiConfig::default()
    };
    assert_eq!(
        initial_burn_per_block(&genesis, &configured),
        UiConfig::default().burn_per_block
    );
    assert_eq!(
        initial_burn_fee(&genesis, &configured),
        UiConfig::default().burn_fee
    );
}

#[test]
fn genesis_default_auto_mining_keeps_burning_after_first_block() {
    let wallet = Wallet::from_seed("genesis-auto-burn-wallet");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
        1,
    )
    .unwrap();
    let mut node = NodeCore::from_ledger_with_burn_fee(
        wallet.clone(),
        ledger,
        GENESIS_INITIAL_BURN_PER_BLOCK,
        GENESIS_INITIAL_BURN_FEE,
    );

    let first = node.automatic_mine_once(1_000);
    let second = node.automatic_mine_once(2_000);
    let third = node.automatic_mine_once(3_000);

    assert_eq!(first.burned.as_ref().map(|tx| tx.amount()), Some(1));
    assert!(first.block.is_some(), "{first:?}");
    assert_eq!(second.burned.as_ref().map(|tx| tx.amount()), Some(1));
    assert!(second.block.is_some(), "{second:?}");
    assert_eq!(third.burned.as_ref().map(|tx| tx.amount()), Some(1));
    assert!(third.block.is_some(), "{third:?}");
    assert!(
        second.skipped_reason.as_deref().is_none_or(|reason| {
            !reason.contains("block must include at least one burn transaction")
        }),
        "{second:?}"
    );
    assert!(
        node.ledger().balance_of(wallet.address())
            >= BLOCK_REWARD - 3 * (GENESIS_INITIAL_BURN_PER_BLOCK + GENESIS_INITIAL_BURN_FEE)
    );
}

#[test]
fn non_genesis_modes_start_with_configured_burn_rate_and_fee() {
    let configured = UiConfig {
        burn_per_block: 50,
        burn_fee: 3,
        ..UiConfig::default()
    };

    let setup = parse(&[]).unwrap().unwrap();
    assert_eq!(initial_burn_per_block(&setup, &configured), 50);
    assert_eq!(initial_burn_fee(&setup, &configured), 3);

    let join = parse(&["--join", "127.0.0.1:9444"]).unwrap().unwrap();
    assert_eq!(initial_burn_per_block(&join, &configured), 50);
    assert_eq!(initial_burn_fee(&join, &configured), 3);

    assert_eq!(
        initial_burn_per_block(&setup, &UiConfig::default()),
        UiConfig::default().burn_per_block
    );
    assert_eq!(
        initial_burn_fee(&setup, &UiConfig::default()),
        UiConfig::default().burn_fee
    );
}

#[test]
fn genesis_and_join_are_exclusive() {
    let error = parse(&["--genesis", "--join", "127.0.0.1:9444"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("choose either --genesis or --join")
    );

    let error = parse(&["--join", "127.0.0.1:9444", "--genesis"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("choose either --genesis or --join")
    );
}

#[test]
fn http_management_port_can_be_configured() {
    let opts = parse(&["--genesis", "--http", "127.0.0.1:18443"])
        .unwrap()
        .unwrap();
    assert_eq!(opts.http_addr.to_string(), "127.0.0.1:18443");
}

#[test]
fn p2p_announce_port_can_be_configured() {
    let opts = parse(&["--p2p-announce", "203.0.113.10:9444"])
        .unwrap()
        .unwrap();

    assert_eq!(
        opts.p2p_announce_addr,
        Some("203.0.113.10:9444".parse().unwrap())
    );
}

#[test]
fn configured_p2p_announce_addr_uses_cli_before_config() {
    let opts = parse(&["--p2p-announce", "203.0.113.20:9444"])
        .unwrap()
        .unwrap();
    let config = UiConfig {
        p2p_announce_addr: Some("203.0.113.10:9444".to_string()),
        ..UiConfig::default()
    };

    assert_eq!(
        configured_p2p_announce_addr(&opts, &config).unwrap(),
        Some("203.0.113.20:9444".parse().unwrap())
    );
}

#[test]
fn configured_p2p_announce_addr_reads_config_without_cli() {
    let opts = parse(&[]).unwrap().unwrap();
    let config = UiConfig {
        p2p_announce_addr: Some("203.0.113.10:9444".to_string()),
        ..UiConfig::default()
    };

    assert_eq!(
        configured_p2p_announce_addr(&opts, &config).unwrap(),
        Some("203.0.113.10:9444".parse().unwrap())
    );
}

#[test]
fn configured_p2p_announce_addr_rejects_invalid_config() {
    let opts = parse(&[]).unwrap().unwrap();
    let config = UiConfig {
        p2p_announce_addr: Some("not-an-address".to_string()),
        ..UiConfig::default()
    };

    let error = configured_p2p_announce_addr(&opts, &config).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid configured P2P announce address")
    );
}

#[test]
fn configured_p2p_bind_addr_uses_configured_public_port() {
    let opts = parse(&[]).unwrap().unwrap();
    let config = UiConfig {
        p2p_accept_inbound: true,
        p2p_bind_port: 9555,
        ..UiConfig::default()
    };

    assert_eq!(
        configured_p2p_bind_addr(&opts, &config).to_string(),
        "0.0.0.0:9555"
    );
}

#[test]
fn configured_p2p_bind_addr_uses_cli_port_after_config_override() {
    let opts = parse(&["--p2p", "127.0.0.1:9555"]).unwrap().unwrap();
    let config = UiConfig {
        p2p_accept_inbound: true,
        p2p_bind_port: 9555,
        ..UiConfig::default()
    };

    assert_eq!(
        configured_p2p_bind_addr(&opts, &config).to_string(),
        "0.0.0.0:9555"
    );
}

#[test]
fn cli_p2p_port_overrides_config_bind_port() {
    let opts = parse(&["--p2p", "127.0.0.1:9555"]).unwrap().unwrap();
    let mut config = UiConfig {
        p2p_accept_inbound: true,
        p2p_bind_port: 9444,
        ..UiConfig::default()
    };

    assert!(apply_cli_p2p_config_overrides(&opts, &mut config));
    assert_eq!(config.p2p_bind_port, 9555);
}

#[test]
fn http_management_port_defaults_to_iuna_port() {
    let opts = parse(&[]).unwrap().unwrap();
    assert_eq!(opts.http_addr.to_string(), "127.0.0.1:18661");
}

#[test]
fn data_dir_defaults_under_home() {
    let opts = parse(&[]).unwrap().unwrap();
    assert_eq!(opts.data_dir, super::default_data_dir());
    assert!(opts.data_dir.ends_with(".iuna"));
}

#[test]
fn wallet_defaults_under_data_dir() {
    let opts = parse(&["--genesis", "--data-dir", "tmp-node"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.wallet_path(),
        std::path::PathBuf::from("tmp-node/wallet.json")
    );
}

#[test]
fn chain_db_defaults_under_data_dir() {
    let opts = parse(&["--genesis", "--data-dir", "tmp-node"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.chain_db_path(),
        std::path::PathBuf::from("tmp-node/chain.sqlite3")
    );
}

#[test]
fn config_defaults_under_data_dir() {
    let opts = parse(&["--genesis", "--data-dir", "tmp-node"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.config_path(),
        std::path::PathBuf::from("tmp-node/config.json")
    );
}

#[test]
fn wallet_path_can_be_explicit() {
    let opts = parse(&["--genesis", "--wallet", "alice-wallet.json"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.wallet_path(),
        std::path::PathBuf::from("alice-wallet.json")
    );
}

#[test]
fn chain_db_path_can_be_explicit() {
    let opts = parse(&["--genesis", "--chain-db", "alice-chain.sqlite3"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.chain_db_path(),
        std::path::PathBuf::from("alice-chain.sqlite3")
    );
}

#[test]
fn genesis_requires_fresh_wallet_path() {
    let opts = parse(&["--genesis"]).unwrap().unwrap();
    let wallet_path = std::path::Path::new("wallet.json");

    validate_wallet_for_mode(&opts, wallet_path, false).unwrap();
    let error = validate_wallet_for_mode(&opts, wallet_path, true).unwrap_err();
    assert!(error.to_string().contains("requires a fresh wallet path"));

    let setup = parse(&[]).unwrap().unwrap();
    validate_wallet_for_mode(&setup, wallet_path, true).unwrap();
}

#[test]
fn vdf_measurement_extrapolates_to_target() {
    assert_eq!(
        extrapolate_vdf_rounds(
            10_000,
            Duration::from_secs(1),
            Duration::from_millis(VDF_TARGET_BLOCK_MS),
        ),
        3_000_000
    );
    assert_eq!(
        extrapolate_vdf_rounds(
            10_000,
            Duration::from_secs(0),
            Duration::from_millis(VDF_TARGET_BLOCK_MS),
        ),
        3_000_000_000_000_000
    );
}

#[test]
fn vdf_measurement_keeps_sampling_until_elapsed_is_useful() {
    let min_elapsed = Duration::from_millis(1);
    let max_rounds = 1_000_000;
    let (rounds, elapsed) =
        measure_vdf_rounds("iuna-test-vdf-calibration", 1, min_elapsed, max_rounds);

    assert!(rounds >= 1);
    assert!(elapsed >= min_elapsed || rounds >= max_rounds);
    assert!(elapsed > Duration::ZERO);
}

#[tokio::test]
async fn genesis_refuses_to_start_when_chain_database_exists() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let persisted_wallet = Wallet::from_seed("persisted-chain-owner");
    let persisted = ledger_with_one_mined_block(&persisted_wallet);
    store.save(&persisted.snapshot()).unwrap();
    let fresh_wallet = Wallet::from_seed("fresh-start-wallet");
    let opts = parse(&["--genesis", "--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let error = initialize_ledger(&opts, fresh_wallet.address(), &store, opts.p2p_addr)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("already contains a blockchain"),
        "{error:#}"
    );
}

#[tokio::test]
async fn startup_resumes_persisted_chain_without_genesis_flag() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let persisted_wallet = Wallet::from_seed("persisted-chain-owner");
    let persisted = ledger_with_one_mined_block(&persisted_wallet);
    store.save(&persisted.snapshot()).unwrap();
    let fresh_wallet = Wallet::from_seed("fresh-start-wallet");
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let resumed = initialize_ledger(&opts, fresh_wallet.address(), &store, opts.p2p_addr)
        .await
        .unwrap();

    assert_eq!(resumed.status().height, 1);
    assert_eq!(resumed.status().tip_hash, persisted.status().tip_hash);
    assert_eq!(resumed.genesis_hash(), persisted.genesis_hash());
    assert_eq!(resumed.balance_of(fresh_wallet.address()), 0);
}

#[tokio::test]
async fn startup_resumes_persisted_chain_with_network_accepted_future_tip() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let persisted_wallet = Wallet::from_seed("persisted-future-chain-owner");
    let mut persisted = ledger_with_one_spendable_iuna(&persisted_wallet);
    let burn = persisted.build_burn(&persisted_wallet, 1, 0).unwrap();
    persisted.submit_transaction(burn).unwrap();
    let future_tip_ms = iuna::app::now_ms().saturating_add(VDF_TARGET_BLOCK_MS);
    let future_block = persisted
        .mine_next_block(&persisted_wallet, future_tip_ms)
        .unwrap();
    let mut snapshot = persisted.snapshot();
    snapshot.blocks.push(future_block);
    assert!(
        Ledger::from_snapshot(snapshot.clone())
            .unwrap_err()
            .to_string()
            .contains("too far in the future")
    );
    store.save(&snapshot).unwrap();
    let fresh_wallet = Wallet::from_seed("fresh-start-wallet");
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let resumed = initialize_ledger(&opts, fresh_wallet.address(), &store, opts.p2p_addr)
        .await
        .unwrap();

    assert_eq!(resumed.status().height, 1);
    assert_eq!(
        resumed.status().tip_hash,
        snapshot.blocks.last().unwrap().hash
    );
}

#[tokio::test]
async fn persisted_chain_satisfies_join_mode_without_contacting_peer() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let alice = Wallet::from_seed("offline-join-alice");
    let persisted = ledger_with_one_mined_block(&alice);
    store.save(&persisted.snapshot()).unwrap();
    let bob = Wallet::from_seed("offline-join-bob");
    let opts = parse(&[
        "--join",
        "127.0.0.1:1",
        "--chain-db",
        chain_path.to_str().unwrap(),
    ])
    .unwrap()
    .unwrap();

    let resumed = initialize_ledger(&opts, bob.address(), &store, opts.p2p_addr)
        .await
        .unwrap();

    assert_eq!(resumed.status().height, 1);
    assert_eq!(resumed.status().tip_hash, persisted.status().tip_hash);
}

#[tokio::test]
async fn invalid_persisted_chain_is_reported_and_never_replaced() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let connection = Connection::open(&chain_path).unwrap();
    connection
        .execute(
            r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, 4, 'bad-tip', x'00010203', 0)
"#,
            [],
        )
        .unwrap();
    let wallet = Wallet::from_seed("bad-db-wallet");
    let opts = parse(&["--genesis", "--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let error = initialize_ledger(&opts, wallet.address(), &store, opts.p2p_addr)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("failed to parse compact chain snapshot from database"),
        "{error:#}"
    );
}

#[tokio::test]
async fn persistence_loop_saves_new_tip_after_node_changes() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("background-persistence");
    let ledger = ledger_with_one_spendable_iuna(&wallet);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(
        wallet.clone(),
        ledger,
        DEFAULT_BURN_PER_BLOCK,
    )));
    let initial_snapshot = { node.lock().await.chain_snapshot() };
    persist_chain_snapshot(&store, initial_snapshot, false)
        .await
        .unwrap();
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store.clone(),
        ui_config,
        Duration::from_millis(10),
    ));
    {
        let mut node = node.lock().await;
        let burn = node.ledger().build_burn(&wallet, 1, 0).unwrap();
        node.receive_transaction(burn).unwrap();
        node.mine_one_at(1_000).unwrap();
    }

    let expected_tip = node.lock().await.ledger().status().tip_hash;
    let mut restored_tip = None;
    for _ in 0..50 {
        if let Some(snapshot) = store.load().unwrap() {
            restored_tip = snapshot.blocks.last().map(|block| block.hash.clone());
            if restored_tip.as_deref() == Some(expected_tip.as_str()) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    persistence_task.abort();

    assert_eq!(restored_tip.as_deref(), Some(expected_tip.as_str()));
}

#[tokio::test]
async fn persistence_loop_skips_setup_placeholder_chain() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("background-persistence-setup");
    let ledger = Ledger::new(BTreeMap::new(), 1);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(wallet, ledger, 0)));
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store.clone(),
        ui_config,
        Duration::from_millis(10),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    persistence_task.abort();

    assert!(store.load().unwrap().is_none());
}
