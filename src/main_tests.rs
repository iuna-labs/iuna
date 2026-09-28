use std::{collections::BTreeMap, sync::Arc, time::Duration};

use iuna::{
    adapters::{
        chain_store::SqliteChainStore,
        config_store::{self, UiConfig},
        ui_data_store::SqliteUiDataStore,
        wallet_store,
    },
    app::{DEFAULT_BURN_PER_BLOCK, MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID, NodeCore},
    domain::{BLOCK_REWARD, GenesisBurn, Ledger, MICRO_IUNA, VDF_TARGET_BLOCK_MS, Wallet},
};
use rusqlite::Connection;
use tempfile::tempdir;
use tokio::sync::Mutex;

use super::{
    ChainMode, CliOptions, GENESIS_BOOTSTRAP_BALANCE, GENESIS_BOOTSTRAP_BURN_AMOUNT,
    GENESIS_INITIAL_BURN_FEE, GENESIS_INITIAL_BURN_PER_BLOCK, StartupWallet,
    apply_cli_p2p_config_overrides, apply_cli_stratum_config_overrides,
    apply_startup_mining_config_overrides, apply_startup_setup_config_override,
    apply_startup_wallet_endpoint_config_overrides, apply_startup_wallet_password_config,
    automatic_finalization_precheck_time, configured_p2p_announce_addr, configured_p2p_bind_addr,
    configured_stratum_addr, extrapolate_vdf_rounds, help_text, initial_burn_fee,
    initial_burn_per_block, initialize_ledger, load_startup_wallet, measure_vdf_rounds,
    parse_startup_bool_env_value, parse_startup_pow_mining_workers_env_value,
    persist_chain_snapshot, project_ui_data_store, restore_pending_transactions_v2,
    run_chain_persistence_with_interval, setup_ledger, should_defer_sync_checkpoint,
    should_log_automatic_finalization_skip, start_genesis_ledger, validate_wallet_for_mode,
};

fn parse(args: &[&str]) -> anyhow::Result<Option<CliOptions>> {
    CliOptions::parse_from(args.iter().map(|arg| arg.to_string()))
}

#[test]
fn help_mentions_dev_seed_verify_bypass_env() {
    assert!(help_text().contains("IUNA_WALLET_PASSWORD=<password>"));
    assert!(help_text().contains("IUNA_POW_MINING_WORKERS=1..32"));
    assert!(help_text().contains("IUNA_SETUP_COMPLETE=true|false"));
    assert!(help_text().contains("IUNA_AUTOMATIC_BURN_ENABLED=true|false"));
    assert!(help_text().contains("IUNA_POW_MINING_ENABLED=true|false"));
    assert!(help_text().contains("IUNA_DEV_SKIP_SEED_VERIFY=1"));
    assert!(help_text().contains("skip seed verification"));
    assert!(help_text().contains("--stratum <addr:port>"));
    assert!(help_text().contains("--debug"));
}

#[test]
fn active_sync_cannot_defer_chain_checkpoint_past_the_interval() {
    let interval = Duration::from_secs(30);

    assert!(should_defer_sync_checkpoint(
        true,
        Duration::from_secs(29),
        interval
    ));
    assert!(!should_defer_sync_checkpoint(true, interval, interval));
    assert!(!should_defer_sync_checkpoint(
        false,
        Duration::ZERO,
        interval
    ));
}

#[test]
fn local_testnet_compose_uses_one_obvious_test_password() {
    let compose = include_str!("../docker-compose.yml");
    assert_eq!(
        compose
            .matches("IUNA_WALLET_PASSWORD: ${IUNA_TESTNET_PASSWORD:-testtesttest}")
            .count(),
        6
    );
    assert!(!compose.contains("IUNA_BOOTSTRAP_WALLET_PASSWORD"));
    assert!(!compose.contains("IUNA_NODE2_WALLET_PASSWORD"));
    assert!(!compose.contains("IUNA_NODE3_WALLET_PASSWORD"));
    assert!(!compose.contains("IUNA_NODE4_WALLET_PASSWORD"));
    assert!(!compose.contains("IUNA_NODE5_WALLET_PASSWORD"));
    assert!(!compose.contains("IUNA_NODE6_WALLET_PASSWORD"));
}

#[test]
fn local_testnet_bootstrap_only_uses_genesis_for_an_empty_chain() {
    let compose = include_str!("../docker-compose.yml");
    assert!(compose.contains("if [ -s /data/chain.sqlite3 ]; then exec iuna --data-dir /data"));
    assert!(compose.contains("else exec iuna --genesis --data-dir /data"));
}

#[test]
fn management_ui_warns_when_estimated_fees_exceed_amounts() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("feeExceedsAmount('transfer')"));
    assert!(html.contains("feeExceedsAmount('burn')"));
    assert!(javascript.contains("feeExceedsAmount(kind)"));
    assert!(javascript.contains("estimated fee IUNA ${this.amountLabel(fee)} exceeds"));
}

#[test]
fn management_ui_opens_on_the_dashboard_and_exposes_chain_health_thresholds() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("@click=\"setTab('dashboard')\""));
    assert!(html.contains("Current balance"));
    assert!(html.contains("dashboardLastBlockLabel()"));
    assert!(html.contains("dashboardMiningLabel()"));
    assert!(html.contains("dashboardBurnLabel()"));
    assert!(html.contains(".dashboard-card.burning .dashboard-status"));
    assert!(html.contains("basicNetworkStatusLabel()"));
    assert!(javascript.contains("tab: \"dashboard\""));
    assert!(javascript.contains("ageMs < 15 * 60 * 1000"));
    assert!(javascript.contains("ageMs < 20 * 60 * 1000"));
    assert!(javascript.contains(
        "`${this.powMiningWorkers} worker${this.powMiningWorkers === 1 ? \"\" : \"s\"}`"
    ));
    assert!(javascript.contains("`IUNA ${this.amountLabel(this.burnAmount)} / blk`"));
    let dashboard_network_state = javascript
        .split_once("dashboardNetworkState()")
        .and_then(|(_, rest)| rest.split_once("\n    },"))
        .map(|(body, _)| body)
        .expect("dashboard network state helper");
    assert!(dashboard_network_state.contains("state === \"stale\""));
    assert!(dashboard_network_state.contains("state === \"peer errors\""));
    assert!(dashboard_network_state.contains("state === \"banned\""));
}

#[test]
fn management_ui_exposes_complete_peer_details() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("@click=\"openPeerModal(peer)\""));
    assert!(html.contains("Last protocol hello"));
    assert!(html.contains("peerDetail().last_hello?.protocol_version"));
    assert!(html.contains("peerCapabilities(peerDetail())"));
    assert!(html.contains("Health and enforcement"));
    assert!(html.contains("x-show=\"peer.country_code\""));
    assert!(html.contains("peerDetail().country_code || '-'"));
    assert!(javascript.contains("selectedPeerAddress: null"));
    assert!(javascript.contains("peerDetail()"));
    assert!(javascript.contains("closePeerModal()"));
}

#[test]
fn management_ui_uses_an_in_app_transfer_confirmation_and_marks_fee_errors() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("Confirm transfer"));
    assert!(html.contains("@click=\"confirmTransfer\""));
    assert!(html.contains("Transaction size"));
    assert!(html.contains("Network fee"));
    assert!(!html.contains("Verify recipient"));
    assert!(html.contains("error: feeEstimateError('transfer')"));
    assert!(javascript.contains("bytes: Number(estimate.bytes)"));
    assert!(javascript.contains("fee: this.microiunaAmount(estimate.fee)"));
    assert!(javascript.contains("fee_per_byte: transfer.feePerByte"));
    assert!(javascript.contains("async confirmTransfer()"));
    assert!(!javascript.contains("window.confirm"));
}

#[test]
fn management_ui_matches_bech32m_contacts_to_canonical_transaction_addresses() {
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(javascript.contains("canonicalAddressKey(address)"));
    assert!(javascript.contains("this.canonicalAddressKey(savedAddress) === canonical"));
}

#[test]
fn management_ui_opens_contact_editor_from_wallet_addresses() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("wallet-address-link"));
    assert!(html.contains("openAddressContact(txFrom(tx))"));
    assert!(html.contains("openBurnLeaderRanksModal(selectedBlock)"));
    assert!(html.contains("openAddressContact(rank.owner)"));
    assert!(javascript.contains("openAddressContact(address)"));
    assert!(javascript.contains("contactAddress(address)"));
    assert!(javascript.contains("this.addressBookEditingAddress = entry?.address || null"));
}

#[test]
fn management_ui_prefers_the_live_rotating_receive_address() {
    let javascript = include_str!("../www/assets/iuna-ui.js");
    let receive_address = javascript
        .split_once("receiveAddress()")
        .and_then(|(_, rest)| rest.split_once("\n    },"))
        .map(|(body, _)| body)
        .expect("receive address helper");

    assert!(receive_address.contains(
        "return this.status.wallet_receive_address || this.setupWallet.address || \"-\";"
    ));
    assert!(javascript.contains("await navigator.clipboard.writeText(this.receiveAddress())"));
    assert!(javascript.contains("fundedWalletAddresses()"));
    assert!(javascript.contains("Funds are held across ${count} wallet address"));
    assert!(javascript.contains("openWalletAddressesModal()"));
    assert!(!javascript.contains("copyHybridAddress"));
}

#[test]
fn management_ui_only_offers_wallet_optimization_when_relevant_or_requested() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert_eq!(html.matches("@click=\"openOptimizeWallet\"").count(), 2);
    assert!(html.contains("x-show=\"showOptimizeSuggestion()\""));
    assert!(javascript.contains("this.walletUtxoPage.total < 500"));
    assert!(html.contains("id=\"wallet-utxos-title\""));
}

#[test]
fn management_ui_blocks_interaction_while_the_node_is_syncing() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains(":inert=\"syncingNode()\""));
    assert!(html.contains("Synchronizing blockchain"));
    assert!(html.contains("syncProgressPercent()"));
    assert!(javascript.contains("this.networkHealth.state === \"syncing\""));
    assert!(javascript.contains("this.networkHealth.sync_validated_height"));
    assert!(javascript.contains("this.syncingNode() ? 1000 : 5000"));
    assert!(javascript.contains("Syncing ${this.syncCurrentHeight().toLocaleString()} of"));
    assert!(javascript.contains("!this.showingNetworkMigration()"));
    assert!(javascript.contains("!this.chainResetModalOpen"));
    assert!(html.contains("Sync stuck? Reset local chain"));
    assert!(html.contains("@click=\"openChainResetModal\""));
}

#[test]
fn management_ui_block_reward_opens_a_fee_flow_transaction() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("@click=\"openBlockRewardModal(selectedBlock)\""));
    assert!(html.contains("input.rewardFee ? `Paid fee ${index + 1}`"));
    assert!(javascript.contains("blockRewardTransaction(block)"));
    assert!(javascript.contains("blockRewardOutputs(block)"));
    assert!(javascript.contains("label: \"Finalizer reward\""));
    assert!(javascript.contains("label: `Committee reward (slot ${bundle.slot})`"));
}

#[test]
fn management_ui_wallet_transactions_share_the_generic_detail_flow() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    assert!(html.contains("openTransactionModal(tx, { source: 'Wallet' })"));
    assert!(javascript.contains("Array.isArray(tx.rewardFeeInputs)"));
    assert!(javascript.contains("Array.isArray(tx.rewardOutputs)"));
    assert!(javascript.contains("outpoint: { txid: input.signature, index: \"fee\" }"));
}

#[test]
fn transaction_filters_default_to_every_transaction_type() {
    let management_javascript = include_str!("../www/assets/iuna-ui.js");
    let wallet_javascript = include_str!("../wallet/app.js");
    let all_filters = "{ transfer: true, mine: true, burn: true, reward: true }";

    assert!(management_javascript.contains(&format!("walletTxFilters: {all_filters}")));
    assert!(wallet_javascript.contains(&format!("const ALL_TRANSACTION_FILTERS = {all_filters}")));
}

#[test]
fn management_ui_uses_the_native_desktop_updater_when_available() {
    let html = include_str!("adapters/http/index_html.rs");
    let javascript = include_str!("../www/assets/iuna-ui.js");
    let tauri_build = include_str!("../src-tauri/build.rs");
    let capability = include_str!("../src-tauri/capabilities/default.json");
    let check_permission =
        include_str!("../src-tauri/permissions/autogenerated/check_desktop_update.toml");
    let install_permission =
        include_str!("../src-tauri/permissions/autogenerated/install_desktop_update.toml");

    assert!(javascript.contains("invoke(\"check_desktop_update\")"));
    assert!(javascript.contains("invoke(\"install_desktop_update\")"));
    assert!(tauri_build.contains("\"check_desktop_update\", \"install_desktop_update\""));
    assert!(capability.contains("\"allow-check-desktop-update\""));
    assert!(capability.contains("\"allow-install-desktop-update\""));
    assert!(check_permission.contains("commands.allow = [\"check_desktop_update\"]"));
    assert!(install_permission.contains("commands.allow = [\"install_desktop_update\"]"));
    assert!(html.contains("Install and restart"));
    assert!(!javascript.contains("window.confirm"));
}

#[test]
fn lightweight_wallet_recent_activity_does_not_use_view_all_filters() {
    let javascript = include_str!("../wallet/app.js");

    assert!(javascript.contains("fetchTransactions(0, 5, ALL_TRANSACTION_FILTERS)"));
    assert!(javascript.contains("activityList(5, false, state.recentTransactions)"));
}

fn ledger_with_one_spendable_iuna(wallet: &Wallet) -> Ledger {
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 3);
    Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1).unwrap()
}

fn ledger_with_one_mined_block(wallet: &Wallet) -> Ledger {
    let mut ledger = ledger_with_one_spendable_iuna(wallet);
    let burn = ledger.build_burn(wallet, 1, 1).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(wallet, 1_000).unwrap();
    ledger.apply_locally_mined_block(block).unwrap();
    ledger
}

fn promoted_candidate_ledger() -> (Ledger, Vec<Wallet>, String) {
    let wallets = [
        Wallet::from_seed("promotion-candidate-alice"),
        Wallet::from_seed("promotion-candidate-bob"),
        Wallet::from_seed("promotion-candidate-carol"),
    ];
    let mut allocations = BTreeMap::new();
    for wallet in &wallets {
        allocations.insert(wallet.address().to_string(), 20 * MICRO_IUNA);
    }
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect(),
        1,
    )
    .unwrap();
    let finalizer_owner = ledger
        .burn_leader_ranks_for_block(ledger.height() + 1)
        .unwrap()
        .first()
        .unwrap()
        .owner
        .clone();
    let finalizer = wallets
        .iter()
        .find(|wallet| wallet.address() == finalizer_owner)
        .unwrap();
    let sender = wallets
        .iter()
        .find(|wallet| wallet.address() != finalizer.address())
        .unwrap();
    let recipient = wallets
        .iter()
        .find(|wallet| {
            wallet.address() != finalizer.address() && wallet.address() != sender.address()
        })
        .unwrap();
    let transfer = ledger
        .build_transfer(sender, recipient.address(), MICRO_IUNA, MICRO_IUNA)
        .unwrap();
    let transfer_signature = transfer.signature().to_string();
    ledger.submit_transaction(transfer).unwrap();
    let anchor = ledger
        .build_burn(finalizer, MICRO_IUNA, MICRO_IUNA)
        .unwrap();
    ledger.submit_transaction(anchor).unwrap();
    let block = ledger
        .mine_next_block(finalizer, VDF_TARGET_BLOCK_MS)
        .unwrap();
    ledger.apply_locally_mined_block(block).unwrap();
    assert!(
        ledger.has_transaction(&transfer_signature),
        "candidate rehearsal chain should contain a transfer before promotion"
    );

    (ledger, wallets.to_vec(), transfer_signature)
}

#[test]
fn encrypted_startup_wallet_loads_as_locked_metadata() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("wallet.json");
    let (wallet, _) =
        wallet_store::replace_with_generated_seed_phrase_encrypted(&path, "password-123456")
            .unwrap();

    let startup = load_startup_wallet(&path, None).unwrap();

    match startup {
        StartupWallet::Locked { address } => assert_eq!(address, wallet.address()),
        StartupWallet::Unlocked { .. } => panic!("encrypted wallet should start locked"),
    }
}

#[test]
fn startup_wallet_password_encrypts_new_wallet_and_configures_auth() {
    let dir = tempdir().unwrap();
    let wallet_path = dir.path().join("wallet.json");
    let config_path = dir.path().join("config.json");
    let password = "local-testnet-password";
    let mut config = UiConfig::default();

    let dirty =
        apply_startup_wallet_password_config(&config_path, &mut config, Some(password)).unwrap();
    config_store::save(&config_path, &config).unwrap();
    let startup = load_startup_wallet(&wallet_path, Some(password)).unwrap();

    assert!(dirty);
    assert!(config.auth_password_hash.is_some());
    assert!(
        wallet_store::metadata(&wallet_path)
            .unwrap()
            .unwrap()
            .encrypted
    );
    match startup {
        StartupWallet::Unlocked { wallet } => {
            let reloaded = wallet_store::load_with_password(&wallet_path, password).unwrap();
            assert_eq!(reloaded.address(), wallet.address());
        }
        StartupWallet::Locked { .. } => panic!("env password should unlock startup wallet"),
    }
}

#[test]
fn startup_wallet_password_unlocks_existing_encrypted_wallet() {
    let dir = tempdir().unwrap();
    let wallet_path = dir.path().join("wallet.json");
    let password = "local-testnet-password";
    let (wallet, _) =
        wallet_store::replace_with_generated_seed_phrase_encrypted(&wallet_path, password).unwrap();

    let startup = load_startup_wallet(&wallet_path, Some(password)).unwrap();

    match startup {
        StartupWallet::Unlocked { wallet: startup } => {
            assert_eq!(startup.address(), wallet.address());
        }
        StartupWallet::Locked { .. } => panic!("env password should unlock encrypted wallet"),
    }
}

#[test]
fn startup_wallet_password_mismatch_does_not_encrypt_plaintext_wallet() {
    let dir = tempdir().unwrap();
    let wallet_path = dir.path().join("wallet.json");
    let config_path = dir.path().join("config.json");
    let (_wallet, _seed) = wallet_store::replace_with_generated_seed_phrase(&wallet_path).unwrap();
    let mut config = UiConfig {
        auth_password_hash: Some(
            iuna::adapters::http::hash_management_password("correct-password").unwrap(),
        ),
        ..UiConfig::default()
    };

    let error =
        apply_startup_wallet_password_config(&config_path, &mut config, Some("wrong-password"))
            .unwrap_err();

    assert!(error.to_string().contains("does not match"));
    assert!(
        !wallet_store::metadata(&wallet_path)
            .unwrap()
            .unwrap()
            .encrypted
    );
    assert!(wallet_store::load_or_create(&wallet_path).is_ok());
}

#[test]
fn startup_mining_env_overrides_persisted_config() {
    let mut config = UiConfig {
        mining_enabled: false,
        pow_mining_enabled: true,
        ..UiConfig::default()
    };

    assert!(apply_startup_mining_config_overrides(
        &mut config,
        Some(true),
        Some(false),
        Some(1),
    ));
    assert!(config.mining_enabled);
    assert!(!config.pow_mining_enabled);
    assert_eq!(config.pow_mining_workers, 1);

    assert!(!apply_startup_mining_config_overrides(
        &mut config,
        Some(true),
        Some(false),
        Some(1),
    ));
}

#[test]
fn startup_mining_env_can_override_genesis_defaults() {
    let mut config = UiConfig {
        mining_enabled: true,
        pow_mining_enabled: false,
        burn_per_block: GENESIS_INITIAL_BURN_PER_BLOCK,
        burn_fee: GENESIS_INITIAL_BURN_FEE,
        ..UiConfig::default()
    };

    assert!(apply_startup_mining_config_overrides(
        &mut config,
        Some(false),
        Some(true),
        Some(1),
    ));
    assert!(!config.mining_enabled);
    assert!(config.pow_mining_enabled);
    assert_eq!(config.burn_per_block, GENESIS_INITIAL_BURN_PER_BLOCK);
    assert_eq!(config.burn_fee, GENESIS_INITIAL_BURN_FEE);
}

#[test]
fn startup_bool_env_parser_accepts_common_boolean_values() {
    for value in ["1", "true", "yes", "on"] {
        assert!(parse_startup_bool_env_value("TEST_BOOL", value).unwrap());
    }
    for value in ["0", "false", "no", "off"] {
        assert!(!parse_startup_bool_env_value("TEST_BOOL", value).unwrap());
    }
    assert!(parse_startup_bool_env_value("TEST_BOOL", "maybe").is_err());
}

#[test]
fn startup_pow_worker_parser_enforces_supported_range() {
    assert_eq!(parse_startup_pow_mining_workers_env_value("1").unwrap(), 1);
    assert_eq!(
        parse_startup_pow_mining_workers_env_value("32").unwrap(),
        32
    );
    assert!(parse_startup_pow_mining_workers_env_value("0").is_err());
    assert!(parse_startup_pow_mining_workers_env_value("33").is_err());
    assert!(parse_startup_pow_mining_workers_env_value("many").is_err());
}

#[test]
fn startup_setup_complete_env_marks_join_and_genesis_nodes_ready() {
    let join = parse(&["--join", "127.0.0.1:9444"]).unwrap().unwrap();
    let mut join_config = UiConfig::default();
    assert!(
        apply_startup_setup_config_override(&join, false, &mut join_config, Some(true)).unwrap()
    );
    assert!(join_config.setup_complete);

    let genesis = parse(&["--genesis"]).unwrap().unwrap();
    let mut genesis_config = UiConfig {
        setup_complete: false,
        mining_enabled: true,
        pow_mining_enabled: false,
        burn_per_block: GENESIS_INITIAL_BURN_PER_BLOCK,
        burn_fee: GENESIS_INITIAL_BURN_FEE,
        ..UiConfig::default()
    };
    assert!(
        apply_startup_setup_config_override(&genesis, false, &mut genesis_config, Some(true))
            .unwrap()
    );
    assert!(genesis_config.setup_complete);
}

#[test]
fn startup_setup_complete_env_rejects_empty_setup_node() {
    let setup = parse(&[]).unwrap().unwrap();
    let mut config = UiConfig::default();
    let error =
        apply_startup_setup_config_override(&setup, false, &mut config, Some(true)).unwrap_err();

    assert!(error.to_string().contains("requires --genesis, --join"));
    assert!(!config.setup_complete);
}

#[test]
fn no_args_starts_setup_mode() {
    let opts = parse(&[]).unwrap().unwrap();
    assert_eq!(opts.chain_mode, ChainMode::Setup);
    assert!(opts.join_peers.is_empty());
}

#[test]
fn setup_ledger_uses_the_requested_address_network_profile() {
    assert_eq!(
        setup_ledger(false).launch_profile().profile_id,
        "iuna-mainnet-candidate"
    );
    assert_eq!(
        setup_ledger(true).launch_profile().profile_id,
        if cfg!(feature = "e2e") {
            "iuna-local-e2e-5s-v1"
        } else {
            "iuna-local-testnet-v1"
        }
    );
}

#[test]
fn stratum_port_can_be_configured() {
    let opts = parse(&["--stratum", "127.0.0.1:3333"]).unwrap().unwrap();
    assert_eq!(opts.stratum_addr, Some("127.0.0.1:3333".parse().unwrap()));
}

#[test]
fn wallet_endpoint_uses_a_separate_configurable_listener() {
    let opts = parse(&["--wallet-endpoint", "0.0.0.0:28662"])
        .unwrap()
        .unwrap();
    assert_eq!(
        opts.wallet_endpoint_addr,
        Some("0.0.0.0:28662".parse().unwrap())
    );
    assert!(help_text().contains("--wallet-endpoint <addr:port>"));
    assert!(help_text().contains("IUNA_WALLET_ENDPOINT_PORT"));
}

#[test]
fn wallet_endpoint_rejects_zero_port() {
    let error = parse(&["--wallet-endpoint", "0.0.0.0:0"]).unwrap_err();
    assert!(error.to_string().contains("between 1 and 65535"));
}

#[test]
fn wallet_endpoint_environment_settings_are_persistable() {
    let mut config = UiConfig::default();
    assert!(apply_startup_wallet_endpoint_config_overrides(
        &mut config,
        Some(true),
        Some(28662),
    ));
    assert!(config.wallet_endpoint_enabled);
    assert_eq!(config.wallet_endpoint_bind_port, 28662);
    assert!(!apply_startup_wallet_endpoint_config_overrides(
        &mut config,
        Some(true),
        Some(28662),
    ));
}

#[test]
fn configured_stratum_addr_uses_settings_when_enabled() {
    let opts = parse(&[]).unwrap().unwrap();
    let config = UiConfig {
        stratum_enabled: true,
        stratum_bind_port: 3334,
        ..UiConfig::default()
    };

    assert_eq!(
        configured_stratum_addr(&opts, &config).unwrap().to_string(),
        "0.0.0.0:3334"
    );
}

#[test]
fn configured_stratum_addr_prefers_cli_addr() {
    let opts = parse(&["--stratum", "127.0.0.1:3335"]).unwrap().unwrap();
    let config = UiConfig {
        stratum_enabled: true,
        stratum_bind_port: 3334,
        ..UiConfig::default()
    };

    assert_eq!(
        configured_stratum_addr(&opts, &config).unwrap().to_string(),
        "127.0.0.1:3335"
    );
}

#[test]
fn debug_logging_can_be_enabled() {
    assert!(!parse(&[]).unwrap().unwrap().debug);
    assert!(parse(&["--debug"]).unwrap().unwrap().debug);
}

#[test]
fn automatic_pow_worker_searches_outside_node_lock() {
    let main_rs = include_str!("main.rs");
    let worker = main_rs
        .split("async fn run_automatic_pow_miner")
        .nth(1)
        .expect("automatic PoW worker should exist");

    assert!(worker.contains("prepare_automatic_pow_mining_job"));
    assert!(worker.contains("tokio::task::spawn_blocking"));
    assert!(worker.contains("finish_automatic_pow_mining_job"));
    assert!(!worker.contains("prepare_automatic_pow_mining()"));
}

#[test]
fn automatic_miners_require_a_good_peer_connection() {
    let main_rs = include_str!("main.rs");
    let finalizer = main_rs
        .split("async fn run_automatic_finalizer")
        .nth(1)
        .and_then(|source| {
            source
                .split("fn automatic_finalization_precheck_time")
                .next()
        })
        .expect("automatic finalizer should exist");
    let pow_miner = main_rs
        .split("async fn run_automatic_pow_miner")
        .nth(1)
        .and_then(|source| source.split("async fn run_peer_sync").next())
        .expect("automatic PoW miner should exist");

    assert!(finalizer.contains("has_good_connection_at"));
    assert!(pow_miner.contains("has_good_connection_at"));
}

#[test]
fn automatic_finalization_skip_logging_only_reports_state_changes() {
    let mut last = None;

    assert!(should_log_automatic_finalization_skip(
        &mut last,
        54,
        "skipped before VDF: block timestamp is too far in the future",
    ));
    assert!(!should_log_automatic_finalization_skip(
        &mut last,
        54,
        "skipped before VDF: block timestamp is too far in the future",
    ));
    assert!(should_log_automatic_finalization_skip(
        &mut last,
        55,
        "skipped before VDF: block timestamp is too far in the future",
    ));
    assert!(should_log_automatic_finalization_skip(
        &mut last,
        55,
        "waiting for burn bundles",
    ));
}

#[test]
fn automatic_finalization_prechecks_future_fallback_at_its_publish_time() {
    let now_ms = 1_000;
    let publish_at_ms = now_ms + 20 * 60 * 1_000;

    assert_eq!(
        automatic_finalization_precheck_time(now_ms, publish_at_ms),
        publish_at_ms
    );
    assert_eq!(
        automatic_finalization_precheck_time(publish_at_ms, now_ms),
        publish_at_ms
    );
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
fn candidate_genesis_mode_cannot_create_a_second_network() {
    let wallet = Wallet::from_seed("second-candidate-genesis");
    let error = start_genesis_ledger(wallet.address(), false).unwrap_err();

    assert!(error.to_string().contains("genesis is pinned"));
    assert!(error.to_string().contains("use --join"));
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
fn genesis_bootstrap_allocation_is_fully_burned_before_reward() {
    let wallet = Wallet::from_seed("fully-burned-genesis-bootstrap");
    let allocations = BTreeMap::from([(wallet.address().to_string(), GENESIS_BOOTSTRAP_BALANCE)]);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(
            wallet.address(),
            GENESIS_BOOTSTRAP_BURN_AMOUNT,
        )],
        1,
    )
    .unwrap();

    assert_eq!(GENESIS_BOOTSTRAP_BALANCE, GENESIS_BOOTSTRAP_BURN_AMOUNT);
    assert_eq!(ledger.balance_of(wallet.address()), BLOCK_REWARD);
}

#[test]
fn genesis_default_auto_mining_keeps_burning_after_first_block() {
    let wallet = Wallet::from_seed("genesis-auto-burn-wallet");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
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

    assert_eq!(
        first.burned.as_ref().map(|tx| tx.amount()),
        Some(GENESIS_INITIAL_BURN_PER_BLOCK)
    );
    assert!(first.block.is_some(), "{first:?}");
    assert_eq!(
        second.burned.as_ref().map(|tx| tx.amount()),
        Some(GENESIS_INITIAL_BURN_PER_BLOCK)
    );
    assert!(second.block.is_some(), "{second:?}");
    assert_eq!(
        third.burned.as_ref().map(|tx| tx.amount()),
        Some(GENESIS_INITIAL_BURN_PER_BLOCK)
    );
    assert!(third.block.is_some(), "{third:?}");
    assert!(
        second.skipped_reason.as_deref().is_none_or(|reason| {
            !reason.contains("block must include at least one burn transaction")
        }),
        "{second:?}"
    );
    assert!(
        node.ledger().balance_of(wallet.address())
            >= BLOCK_REWARD
                .saturating_sub(3 * (GENESIS_INITIAL_BURN_PER_BLOCK + GENESIS_INITIAL_BURN_FEE))
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
fn cli_stratum_addr_enables_and_persists_config_port() {
    let opts = parse(&["--stratum", "127.0.0.1:3335"]).unwrap().unwrap();
    let mut config = UiConfig::default();

    assert!(apply_cli_stratum_config_overrides(&opts, &mut config));
    assert!(config.stratum_enabled);
    assert_eq!(config.stratum_bind_port, 3335);
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
    let rounds_per_target = 10_000_u64.saturating_mul(VDF_TARGET_BLOCK_MS) / 1_000;
    let rounds_from_zero_elapsed = 10_000_u64
        .saturating_mul(VDF_TARGET_BLOCK_MS)
        .saturating_mul(1_000_000);
    assert_eq!(
        extrapolate_vdf_rounds(
            10_000,
            Duration::from_secs(1),
            Duration::from_millis(VDF_TARGET_BLOCK_MS),
        ),
        rounds_per_target
    );
    assert_eq!(
        extrapolate_vdf_rounds(
            10_000,
            Duration::from_secs(0),
            Duration::from_millis(VDF_TARGET_BLOCK_MS),
        ),
        rounds_from_zero_elapsed
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

    let error = initialize_ledger(
        &opts,
        fresh_wallet.address(),
        &store,
        opts.p2p_addr,
        false,
        false,
    )
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

    let resumed = initialize_ledger(
        &opts,
        fresh_wallet.address(),
        &store,
        opts.p2p_addr,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(resumed.status().height, 1);
    assert_eq!(resumed.status().tip_hash, persisted.status().tip_hash);
    assert_eq!(resumed.genesis_hash(), persisted.genesis_hash());
    assert_eq!(resumed.balance_of(fresh_wallet.address()), 0);
}

#[tokio::test]
async fn startup_rejects_unpinned_candidate_genesis() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let wallet = Wallet::from_seed("unpinned-candidate-genesis");
    let persisted = ledger_with_one_mined_block(&wallet);
    store.save(&persisted.snapshot()).unwrap();
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let error = initialize_ledger(&opts, wallet.address(), &store, opts.p2p_addr, false, true)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("does not match pinned genesis"));
}

#[tokio::test]
async fn startup_rebuilds_state_from_a_locally_trusted_chain() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let wallet = Wallet::from_seed("trusted-persisted-chain-owner");
    let persisted = ledger_with_one_mined_block(&wallet);
    store.save_verified(&persisted.snapshot()).unwrap();
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let resumed = initialize_ledger(&opts, wallet.address(), &store, opts.p2p_addr, false, false)
        .await
        .unwrap();

    assert_eq!(resumed.status(), persisted.status());
    assert_eq!(
        resumed.balance_of(wallet.address()),
        persisted.balance_of(wallet.address())
    );
    assert_eq!(resumed.snapshot(), persisted.snapshot());
}

#[test]
fn consensus_migration_revalidates_only_from_its_activation_height() {
    let wallet = Wallet::from_seed("consensus-revalidation-boundary-owner");
    let mut snapshot = ledger_with_one_mined_block(&wallet).snapshot();
    snapshot.blocks[1].leader_proof.as_mut().unwrap().signature = "0".repeat(128);

    assert!(Ledger::from_persisted_snapshot_revalidating_from(snapshot.clone(), Some(1)).is_err());
    let trusted_prefix =
        Ledger::from_persisted_snapshot_revalidating_from(snapshot, Some(2)).unwrap();
    assert_eq!(trusted_prefix.height(), 1);
}

#[tokio::test]
async fn local_testnet_requests_reset_for_persisted_normal_launch_profile() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let persisted_wallet = Wallet::from_seed("normal-profile-chain-owner");
    let persisted = ledger_with_one_mined_block(&persisted_wallet);
    store.save(&persisted.snapshot()).unwrap();
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let initialized = initialize_ledger(
        &opts,
        persisted_wallet.address(),
        &store,
        opts.p2p_addr,
        true,
        false,
    )
    .await
    .unwrap();

    assert_eq!(
        initialized.migration_from.as_deref(),
        Some("iuna-mainnet-candidate")
    );
    assert!(initialized.ledger.is_setup_placeholder());
}

#[tokio::test]
async fn startup_requests_reset_for_a_legacy_network_profile() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let wallet = Wallet::from_seed("legacy-network-owner");
    let mut snapshot = ledger_with_one_mined_block(&wallet).snapshot();
    snapshot.launch_profile.profile_id = "iuna-devnet-v5".to_string();
    store.save(&snapshot).unwrap();
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let initialized =
        initialize_ledger(&opts, wallet.address(), &store, opts.p2p_addr, false, false)
            .await
            .unwrap();

    assert_eq!(
        initialized.migration_from.as_deref(),
        Some("iuna-devnet-v5")
    );
    assert!(initialized.ledger.is_setup_placeholder());
    assert_eq!(
        store.load().unwrap().unwrap().launch_profile.profile_id,
        "iuna-devnet-v5"
    );
}

#[tokio::test]
async fn candidate_promotion_reuses_chain_data_and_can_continue_mining() {
    assert_eq!(MAINNET_CANDIDATE_NETWORK_ID, "iuna-mainnet-candidate");
    assert_eq!(MAINNET_NETWORK_ID, "iuna-mainnet-v1");
    assert_ne!(MAINNET_CANDIDATE_NETWORK_ID, MAINNET_NETWORK_ID);
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let (candidate, wallets, transfer_signature) = promoted_candidate_ledger();
    let candidate_genesis = candidate.genesis_hash().to_string();
    let candidate_tip = candidate.tip_hash().to_string();
    let candidate_profile = candidate.launch_profile().clone();
    let candidate_utxos = candidate.all_utxos();
    let candidate_next_ranks = candidate
        .burn_leader_ranks_for_block(candidate.height() + 1)
        .unwrap();
    store.save(&candidate.snapshot()).unwrap();
    let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
        .unwrap()
        .unwrap();

    let mut promoted = initialize_ledger(
        &opts,
        wallets[0].address(),
        &store,
        opts.p2p_addr,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(promoted.genesis_hash(), candidate_genesis);
    assert_eq!(promoted.tip_hash(), candidate_tip);
    assert_eq!(promoted.launch_profile(), &candidate_profile);
    assert_eq!(promoted.all_utxos(), candidate_utxos);
    assert_eq!(
        promoted
            .burn_leader_ranks_for_block(promoted.height() + 1)
            .unwrap(),
        candidate_next_ranks
    );
    assert!(promoted.has_transaction(&transfer_signature));

    let next_owner = promoted
        .burn_leader_ranks_for_block(promoted.height() + 1)
        .unwrap()
        .first()
        .unwrap()
        .owner
        .clone();
    let next_finalizer = wallets
        .iter()
        .find(|wallet| wallet.address() == next_owner)
        .unwrap();
    let anchor = promoted
        .build_burn(next_finalizer, MICRO_IUNA, MICRO_IUNA)
        .unwrap();
    promoted.submit_transaction(anchor).unwrap();
    let next_block = promoted
        .mine_next_block(
            next_finalizer,
            promoted
                .chain()
                .last()
                .unwrap()
                .timestamp_ms
                .saturating_add(VDF_TARGET_BLOCK_MS),
        )
        .unwrap();
    promoted.apply_locally_mined_block(next_block).unwrap();

    assert_eq!(promoted.genesis_hash(), candidate_genesis);
    assert_eq!(promoted.height(), candidate.height() + 1);
    assert_ne!(promoted.tip_hash(), candidate_tip);
}

#[tokio::test]
async fn startup_resumes_persisted_chain_with_network_accepted_future_tip() {
    let dir = tempdir().unwrap();
    let chain_path = dir.path().join("chain.sqlite3");
    let store = SqliteChainStore::open(&chain_path).unwrap();
    let persisted_wallet = Wallet::from_seed("persisted-future-chain-owner");
    let mut persisted = ledger_with_one_spendable_iuna(&persisted_wallet);
    let burn = persisted.build_burn(&persisted_wallet, 1, 1).unwrap();
    persisted.submit_transaction(burn).unwrap();
    let future_tip_ms = iuna::app::now_ms().saturating_add(10 * 60 * 1_000);
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

    let resumed = initialize_ledger(
        &opts,
        fresh_wallet.address(),
        &store,
        opts.p2p_addr,
        false,
        false,
    )
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

    let resumed = initialize_ledger(&opts, bob.address(), &store, opts.p2p_addr, false, false)
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

    let error = initialize_ledger(&opts, wallet.address(), &store, opts.p2p_addr, false, false)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("failed to parse compact chain snapshot from database"),
        "{error:#}"
    );
}

#[test]
fn startup_prunes_a_persisted_v2_transaction_that_no_longer_validates() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("stale-persisted-v2");
    let ledger = ledger_with_one_spendable_iuna(&wallet);
    let domain = ledger.transaction_v2_domain().unwrap();
    let transaction = ledger.build_v2_migration_batch(&wallet, 1).unwrap();
    let to_hex =
        |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    let transaction_id = to_hex(&transaction.transaction_id(&domain).unwrap());
    let envelope = to_hex(&transaction.encode(&domain).unwrap());
    store
        .save_pending_transaction_v2(&transaction_id, &envelope)
        .unwrap();
    let mut node = NodeCore::from_ledger(wallet, ledger, DEFAULT_BURN_PER_BLOCK);

    let restored = restore_pending_transactions_v2(&mut node, &store).unwrap();

    assert_eq!(restored, 0);
    assert!(node.pending_transaction_v2_envelopes().unwrap().is_empty());
    assert!(store.load_pending_transactions_v2().unwrap().is_empty());
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
    let initial_tip = initial_snapshot
        .blocks
        .last()
        .map(|block| block.hash.clone());
    persist_chain_snapshot(&store, initial_snapshot)
        .await
        .unwrap();
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));
    let ui_data_store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store.clone(),
        ui_data_store.clone(),
        ui_config,
        Duration::from_millis(10),
        initial_tip,
        false,
    ));
    {
        let mut node = node.lock().await;
        let burn = node.ledger().build_burn(&wallet, 1, 1).unwrap();
        node.receive_transaction(burn).unwrap();
        node.mine_one_at(1_000).unwrap();
    }

    let expected_tip = node.lock().await.ledger().status().tip_hash;
    let (restored_tip, projected_tip) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let restored_tip = store
                .load()
                .unwrap()
                .and_then(|snapshot| snapshot.blocks.last().map(|block| block.hash.clone()));
            let projected_tip = Connection::open(ui_data_store.path())
                .unwrap()
                .query_row(
                    "SELECT tip_hash FROM ui_cache_meta WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            if restored_tip.as_deref() == Some(expected_tip.as_str())
                && projected_tip.as_deref() == Some(expected_tip.as_str())
            {
                return (restored_tip, projected_tip);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let restored_tip = store
            .load()
            .unwrap()
            .and_then(|snapshot| snapshot.blocks.last().map(|block| block.hash.clone()));
        let projected_tip = Connection::open(ui_data_store.path())
            .unwrap()
            .query_row(
                "SELECT tip_hash FROM ui_cache_meta WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok();
        (restored_tip, projected_tip)
    });
    persistence_task.abort();

    assert_eq!(restored_tip.as_deref(), Some(expected_tip.as_str()));
    assert_eq!(projected_tip.as_deref(), Some(expected_tip.as_str()));
    assert!(ui_data_store.load_metrics().unwrap().is_empty());
}

#[tokio::test]
async fn persistence_loop_skips_tip_already_projected_at_startup() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let ui_data_store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("background-persistence-warmed");
    let ledger = ledger_with_one_spendable_iuna(&wallet);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(
        wallet,
        ledger,
        DEFAULT_BURN_PER_BLOCK,
    )));
    let initial_snapshot = { node.lock().await.chain_snapshot() };
    let initial_tip = initial_snapshot
        .blocks
        .last()
        .map(|block| block.hash.clone());
    persist_chain_snapshot(&store, initial_snapshot.clone())
        .await
        .unwrap();
    project_ui_data_store(&ui_data_store, initial_snapshot, false)
        .await
        .unwrap();
    let ui_data_connection = Connection::open(ui_data_store.path()).unwrap();
    ui_data_connection
        .execute(
            "UPDATE ui_cache_meta SET updated_at_ms = 123 WHERE id = 1",
            [],
        )
        .unwrap();
    drop(ui_data_connection);
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store,
        ui_data_store.clone(),
        ui_config,
        Duration::from_millis(10),
        initial_tip,
        false,
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    persistence_task.abort();

    let ui_data_connection = Connection::open(ui_data_store.path()).unwrap();
    let updated_at_ms: u64 = ui_data_connection
        .query_row(
            "SELECT updated_at_ms FROM ui_cache_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(updated_at_ms, 123);
}

#[tokio::test]
async fn persistence_loop_projects_metrics_when_mode_changes_without_a_new_tip() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let ui_data_store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("background-metrics-mode-change");
    let ledger = ledger_with_one_spendable_iuna(&wallet);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(
        wallet,
        ledger,
        DEFAULT_BURN_PER_BLOCK,
    )));
    let initial_snapshot = { node.lock().await.chain_snapshot() };
    let initial_tip = initial_snapshot
        .blocks
        .last()
        .map(|block| block.hash.clone());
    persist_chain_snapshot(&store, initial_snapshot.clone())
        .await
        .unwrap();
    project_ui_data_store(&ui_data_store, initial_snapshot, false)
        .await
        .unwrap();
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store,
        ui_data_store.clone(),
        Arc::clone(&ui_config),
        Duration::from_millis(10),
        initial_tip,
        false,
    ));
    ui_config.lock().await.keep_track_of_metrics = true;

    tokio::time::timeout(Duration::from_secs(5), async {
        while ui_data_store.load_metrics().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    persistence_task.abort();

    assert!(!ui_data_store.load_metrics().unwrap().is_empty());
}

#[tokio::test]
async fn persistence_loop_skips_setup_placeholder_chain() {
    let dir = tempdir().unwrap();
    let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
    let ui_data_store = SqliteUiDataStore::open(dir.path().join("ui_data.sqlite3")).unwrap();
    let wallet = Wallet::from_seed("background-persistence-setup");
    let ledger = Ledger::new(BTreeMap::new(), 1);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(wallet, ledger, 0)));
    let ui_config = Arc::new(Mutex::new(UiConfig::default()));

    let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
        Arc::clone(&node),
        store.clone(),
        ui_data_store.clone(),
        ui_config,
        Duration::from_millis(10),
        None,
        false,
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    persistence_task.abort();

    assert!(store.load().unwrap().is_none());
    assert!(ui_data_store.load_metrics().unwrap().is_empty());
}
