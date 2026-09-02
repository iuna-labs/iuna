use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use iuna::{
    adapters::{
        chain_store::SqliteChainStore, config_store, http, p2p, stratum,
        ui_data_store::SqliteUiDataStore, wallet_store,
    },
    app::{
        NodeCore, PeerBook, SharedNode, SharedPeerBook, StratumStatus, debug_logging_enabled,
        now_ms, set_debug_logging,
    },
    domain::{
        Amount, ChainSnapshot, GenesisBurn, LaunchProfile, Ledger, MAX_VDF_ROUNDS, MICRO_IUNA,
        VDF_TARGET_BLOCK_MS, VdfProgress, VdfProgressPhase, run_vdf,
        run_vdf_cancellable_with_progress,
    },
};
use tokio::sync::Mutex;

mod cli;
use cli::{
    ChainMode, CliOptions, apply_cli_p2p_config_overrides, apply_cli_stratum_config_overrides,
    configured_p2p_announce_addr, configured_p2p_bind_addr, configured_stratum_addr,
    initial_burn_fee, initial_burn_per_block, validate_wallet_for_mode,
};
#[cfg(test)]
use cli::{default_data_dir, help_text};

const GENESIS_BOOTSTRAP_BALANCE: Amount = MICRO_IUNA;
const GENESIS_BOOTSTRAP_BURN_AMOUNT: Amount = MICRO_IUNA;
const GENESIS_INITIAL_BURN_PER_BLOCK: Amount = config_store::DEFAULT_BURN_AMOUNT;
const GENESIS_INITIAL_BURN_FEE: Amount = config_store::DEFAULT_BURN_FEE;
const VDF_MEASUREMENT_INITIAL_ROUNDS: u64 = 1_000;
const VDF_MEASUREMENT_MAX_ROUNDS: u64 = 10_000_000;
const VDF_MEASUREMENT_MIN_ELAPSED: Duration = Duration::from_millis(150);
const VDF_PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(10);
const SYNC_CHAIN_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);
const AUTOMATIC_BURN_ENABLED_ENV: &str = "IUNA_AUTOMATIC_BURN_ENABLED";
const POW_MINING_ENABLED_ENV: &str = "IUNA_POW_MINING_ENABLED";
const POW_MINING_WORKERS_ENV: &str = "IUNA_POW_MINING_WORKERS";
const LOCAL_TESTNET_ENV: &str = "IUNA_LOCAL_TESTNET";
const SETUP_COMPLETE_ENV: &str = "IUNA_SETUP_COMPLETE";
const WALLET_PASSWORD_ENV: &str = "IUNA_WALLET_PASSWORD";

#[tokio::main]
async fn main() -> Result<()> {
    let Some(opts) = CliOptions::parse()? else {
        return Ok(());
    };
    let startup_local_testnet = startup_bool_from_env(LOCAL_TESTNET_ENV)?.unwrap_or(false);
    #[cfg(feature = "e2e")]
    if !startup_local_testnet {
        bail!("the e2e build requires {LOCAL_TESTNET_ENV}=true");
    }
    set_debug_logging(opts.debug);
    let debug_logging = opts.debug;
    let wallet_path = opts.wallet_path();
    let config_path = opts.config_path();
    let wallet_file_exists = wallet_path.exists();
    validate_wallet_for_mode(&opts, &wallet_path, wallet_file_exists)?;
    let chain_db_path = opts.chain_db_path();
    let ui_data_db_path = ui_data_db_path(&chain_db_path);
    let chain_store = SqliteChainStore::open(&chain_db_path)?;
    let ui_data_store = SqliteUiDataStore::open(&ui_data_db_path)?;
    let persisted_chain_exists = chain_store.contains_chain()?;
    if opts.chain_mode == ChainMode::Genesis && persisted_chain_exists {
        bail!(
            "--genesis refuses to run because chain database already contains a blockchain at {}; start without --genesis to resume it",
            chain_store.path().display()
        );
    }
    let mut ui_config = config_store::load_or_create(&config_path)?;
    let startup_wallet_password = startup_wallet_password_from_env()?;
    let startup_automatic_burn_enabled = startup_bool_from_env(AUTOMATIC_BURN_ENABLED_ENV)?;
    let startup_pow_mining_enabled = startup_bool_from_env(POW_MINING_ENABLED_ENV)?;
    let startup_pow_mining_workers = startup_pow_mining_workers_from_env()?;
    let startup_setup_complete = startup_bool_from_env(SETUP_COMPLETE_ENV)?;
    let p2p_config_dirty = apply_cli_p2p_config_overrides(&opts, &mut ui_config);
    let stratum_config_dirty = apply_cli_stratum_config_overrides(&opts, &mut ui_config);
    let p2p_announce_addr = configured_p2p_announce_addr(&opts, &ui_config)?;
    let configured_p2p_addr = configured_p2p_bind_addr(&opts, &ui_config);
    let configured_stratum_addr = configured_stratum_addr(&opts, &ui_config);
    let p2p_accept_inbound = ui_config.p2p_accept_inbound;
    let advertised_p2p_addr = p2p_announce_addr.unwrap_or(configured_p2p_addr);
    if opts.chain_mode == ChainMode::Genesis {
        ui_config.setup_complete = false;
        ui_config.mining_enabled = true;
        ui_config.pow_mining_enabled = false;
        ui_config.burn_per_block = GENESIS_INITIAL_BURN_PER_BLOCK;
        ui_config.burn_fee = GENESIS_INITIAL_BURN_FEE;
    }
    let mining_config_dirty = apply_startup_mining_config_overrides(
        &mut ui_config,
        startup_automatic_burn_enabled,
        startup_pow_mining_enabled,
        startup_pow_mining_workers,
    );
    let setup_config_dirty = apply_startup_setup_config_override(
        &opts,
        persisted_chain_exists,
        &mut ui_config,
        startup_setup_complete,
    )?;
    let auth_config_dirty = apply_startup_wallet_password_config(
        &config_path,
        &mut ui_config,
        startup_wallet_password.as_deref(),
    )?;
    let wallet_load = load_startup_wallet(&wallet_path, startup_wallet_password.as_deref())?;
    let wallet_address = wallet_load.address().to_string();
    let ui_config_dirty = opts.chain_mode == ChainMode::Genesis
        || p2p_config_dirty
        || stratum_config_dirty
        || mining_config_dirty
        || setup_config_dirty;
    if ui_config_dirty || auth_config_dirty {
        config_store::save(&config_path, &ui_config)?;
    }
    let initialized_ledger = initialize_ledger(
        &opts,
        &wallet_address,
        &chain_store,
        advertised_p2p_addr,
        startup_local_testnet,
    )
    .await?;
    let migration_from = initialized_ledger.migration_from.clone();
    let migration_required = migration_from.is_some();
    let ledger = initialized_ledger.ledger;
    let has_chain = migration_from.is_none() && (opts.has_chain() || persisted_chain_exists);
    let initial_burn_per_block = initial_burn_per_block(&opts, &ui_config);
    let initial_burn_fee = initial_burn_fee(&opts, &ui_config);

    let mut node_core = match wallet_load {
        StartupWallet::Unlocked { wallet } => NodeCore::from_ledger_with_burn_fee_and_enabled(
            wallet,
            ledger,
            ui_config.mining_enabled,
            initial_burn_per_block,
            initial_burn_fee,
        ),
        StartupWallet::Locked { address } => NodeCore::from_locked_wallet_address(
            address,
            ledger,
            ui_config.mining_enabled,
            initial_burn_per_block,
            initial_burn_fee,
        ),
    };
    node_core.set_pow_mining_workers(ui_config.pow_mining_workers);
    node_core.set_pow_mining_enabled(ui_config.pow_mining_enabled);
    node_core.set_recovery_vdf_top_rank_percent(ui_config.recovery_vdf_top_rank_percent);
    if let Some(from_network) = migration_from {
        println!(
            "network upgrade requires local chain reset: {from_network} -> {}",
            iuna::app::NETWORK_ID
        );
        node_core.require_network_migration(from_network);
    }
    let node: SharedNode = Arc::new(Mutex::new(node_core));
    let ui_config = Arc::new(Mutex::new(ui_config));
    let mut peers = ui_config.lock().await.peers.clone();
    peers.extend(opts.peers);
    let peers: SharedPeerBook = Arc::new(Mutex::new(PeerBook::from_addresses(peers)));
    if has_chain {
        let initial_snapshot = { node.lock().await.chain_snapshot() };
        let keep_metrics = ui_config.lock().await.keep_track_of_metrics;
        persist_chain_snapshot(&chain_store, initial_snapshot.clone()).await?;
        warm_ui_data_store(&ui_data_store, initial_snapshot, keep_metrics).await?;
    } else if !migration_required {
        clear_ui_data_store(&ui_data_store).await?;
    }

    println!("iuna wallet: {}", node.lock().await.wallet_address());
    if node.lock().await.wallet_is_locked() {
        println!("wallet locked: unlock it in the management UI");
    }
    println!("wallet file: {}", wallet_path.display());
    println!("config file: {}", config_path.display());
    println!("chain database: {}", chain_store.path().display());
    println!("UI data database: {}", ui_data_store.path().display());
    println!("management UI: http://{}", opts.http_addr);
    if p2p_accept_inbound {
        println!("p2p listener: {configured_p2p_addr}");
    } else {
        println!("p2p listener: disabled (outbound-only)");
    }
    if p2p_accept_inbound {
        if let Some(addr) = p2p_announce_addr {
            println!("p2p announce address: {addr}");
        }
    }
    println!(
        "automatic finalization: VDF-driven, burning {} IUNA per block with {} IUNA per byte fee rate",
        format_iuna(initial_burn_per_block),
        format_iuna(initial_burn_fee)
    );

    let gossip = p2p::GossipNetwork::start(
        Arc::clone(&node),
        Arc::clone(&peers),
        configured_p2p_addr,
        p2p_announce_addr,
        p2p_accept_inbound,
    )
    .await?;
    let mut stratum_status = StratumStatus {
        enabled: false,
        listen_addr: None,
    };
    if let Some(stratum_addr) = configured_stratum_addr {
        let stratum =
            stratum::StratumServer::start(Arc::clone(&node), gossip.clone(), stratum_addr).await?;
        println!("stratum listener: {}", stratum.listen_addr());
        stratum_status = StratumStatus {
            enabled: true,
            listen_addr: Some(stratum.listen_addr().to_string()),
        };
    }

    let persistence_node = Arc::clone(&node);
    let persistence_store = chain_store.clone();
    let persistence_ui_data_store = ui_data_store.clone();
    let persistence_config = Arc::clone(&ui_config);
    let persistence_gossip = gossip.clone();
    let persistence_initial_tip = {
        let node = node.lock().await;
        if node.has_real_chain() {
            Some(node.chain_tip_hash())
        } else {
            None
        }
    };
    let persistence_initial_keep_metrics = ui_config.lock().await.keep_track_of_metrics;
    tokio::spawn(async move {
        run_chain_persistence(
            persistence_node,
            persistence_store,
            persistence_ui_data_store,
            persistence_config,
            persistence_gossip,
            persistence_initial_tip,
            persistence_initial_keep_metrics,
        )
        .await;
    });

    let finalizer_node = Arc::clone(&node);
    let finalizer_gossip = gossip.clone();
    tokio::spawn(async move {
        run_automatic_finalizer(finalizer_node, finalizer_gossip, debug_logging).await;
    });

    let pow_miner_node = Arc::clone(&node);
    let pow_miner_gossip = gossip.clone();
    tokio::spawn(async move {
        run_automatic_pow_miner(pow_miner_node, pow_miner_gossip, debug_logging).await;
    });

    let sync_node = Arc::clone(&node);
    let sync_gossip = gossip.clone();
    tokio::spawn(async move {
        run_peer_sync(sync_node, sync_gossip, debug_logging).await;
    });

    if !has_chain {
        println!("setup mode: waiting to join or create a chain");
    }

    http::serve(
        node,
        peers,
        gossip,
        ui_config,
        http::ServeOptions {
            config_path,
            chain_store,
            ui_data_store,
            wallet_path,
            stratum: stratum_status,
            addr: opts.http_addr,
        },
    )
    .await
}

enum StartupWallet {
    Unlocked { wallet: iuna::domain::Wallet },
    Locked { address: String },
}

impl StartupWallet {
    fn address(&self) -> &str {
        match self {
            Self::Unlocked { wallet, .. } => wallet.address(),
            Self::Locked { address } => address,
        }
    }
}

fn load_startup_wallet(
    wallet_path: &Path,
    startup_wallet_password: Option<&str>,
) -> Result<StartupWallet> {
    if let Some(password) = startup_wallet_password {
        if wallet_path.exists() {
            wallet_store::encrypt_existing_with_password(wallet_path, password)?;
            let wallet = wallet_store::load_with_password(wallet_path, password)?;
            return Ok(StartupWallet::Unlocked { wallet });
        }
        let (wallet, _) =
            wallet_store::replace_with_generated_seed_phrase_encrypted(wallet_path, password)?;
        return Ok(StartupWallet::Unlocked { wallet });
    }

    match wallet_store::load_or_create(wallet_path) {
        Ok(wallet) => Ok(StartupWallet::Unlocked { wallet }),
        Err(error) => {
            let Some(metadata) = wallet_store::metadata(wallet_path)? else {
                return Err(error);
            };
            if metadata.encrypted {
                Ok(StartupWallet::Locked {
                    address: metadata.address,
                })
            } else {
                Err(error)
            }
        }
    }
}

fn startup_wallet_password_from_env() -> Result<Option<String>> {
    let Some(password) = std::env::var_os(WALLET_PASSWORD_ENV) else {
        return Ok(None);
    };
    let password = password
        .into_string()
        .map_err(|_| anyhow::anyhow!("{WALLET_PASSWORD_ENV} must be valid UTF-8"))?;
    http::validate_management_password(&password)
        .with_context(|| format!("{WALLET_PASSWORD_ENV} is not a valid wallet password"))?;
    Ok(Some(password))
}

fn startup_bool_from_env(name: &str) -> Result<Option<bool>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{name} must be valid UTF-8"))?;
    let normalized = value.trim().to_ascii_lowercase();
    parse_startup_bool_env_value(name, &normalized).map(Some)
}

fn parse_startup_bool_env_value(name: &str, normalized: &str) -> Result<bool> {
    match normalized {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be one of true, false, 1, 0, yes, no, on, or off"),
    }
}

fn startup_pow_mining_workers_from_env() -> Result<Option<u8>> {
    let Some(value) = std::env::var_os(POW_MINING_WORKERS_ENV) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{POW_MINING_WORKERS_ENV} must be valid UTF-8"))?;
    parse_startup_pow_mining_workers_env_value(value.trim()).map(Some)
}

fn parse_startup_pow_mining_workers_env_value(value: &str) -> Result<u8> {
    let workers = value
        .parse::<u8>()
        .with_context(|| format!("{POW_MINING_WORKERS_ENV} must be an integer"))?;
    if !(1..=config_store::MAX_POW_MINING_WORKERS).contains(&workers) {
        bail!(
            "{POW_MINING_WORKERS_ENV} must be between 1 and {}",
            config_store::MAX_POW_MINING_WORKERS
        );
    }
    Ok(workers)
}

fn apply_startup_mining_config_overrides(
    ui_config: &mut config_store::UiConfig,
    automatic_burn_enabled: Option<bool>,
    pow_mining_enabled: Option<bool>,
    pow_mining_workers: Option<u8>,
) -> bool {
    let mut dirty = false;
    if let Some(enabled) = automatic_burn_enabled {
        if ui_config.mining_enabled != enabled {
            ui_config.mining_enabled = enabled;
            dirty = true;
        }
    }
    if let Some(enabled) = pow_mining_enabled {
        if ui_config.pow_mining_enabled != enabled {
            ui_config.pow_mining_enabled = enabled;
            dirty = true;
        }
    }
    if let Some(workers) = pow_mining_workers {
        if ui_config.pow_mining_workers != workers {
            ui_config.pow_mining_workers = workers;
            dirty = true;
        }
    }
    dirty
}

fn apply_startup_setup_config_override(
    opts: &CliOptions,
    persisted_chain_exists: bool,
    ui_config: &mut config_store::UiConfig,
    setup_complete: Option<bool>,
) -> Result<bool> {
    let Some(setup_complete) = setup_complete else {
        return Ok(false);
    };
    if setup_complete
        && opts.chain_mode == ChainMode::Setup
        && !persisted_chain_exists
        && opts.join_peers.is_empty()
    {
        bail!(
            "{SETUP_COMPLETE_ENV}=true requires --genesis, --join, or an existing chain database"
        );
    }
    if ui_config.setup_complete == setup_complete {
        return Ok(false);
    }
    ui_config.setup_complete = setup_complete;
    Ok(true)
}

fn apply_startup_wallet_password_config(
    config_path: &Path,
    ui_config: &mut config_store::UiConfig,
    password: Option<&str>,
) -> Result<bool> {
    let Some(password) = password else {
        return Ok(false);
    };
    let Some(existing_hash) = ui_config.auth_password_hash.as_deref() else {
        ui_config.auth_password_hash = Some(http::hash_management_password(password)?);
        return Ok(true);
    };
    if !http::verify_management_password(password, existing_hash)? {
        bail!(
            "{WALLET_PASSWORD_ENV} does not match the configured management UI and wallet password in {}",
            config_path.display()
        );
    }
    Ok(false)
}

fn format_iuna(amount: Amount) -> String {
    let whole = amount / MICRO_IUNA;
    let fractional = amount % MICRO_IUNA;
    if fractional == 0 {
        whole.to_string()
    } else {
        let mut fractional = format!("{fractional:06}");
        while fractional.ends_with('0') {
            fractional.pop();
        }
        format!("{whole}.{fractional}")
    }
}

fn ui_data_db_path(chain_db_path: &Path) -> PathBuf {
    chain_db_path.with_file_name("ui_data.sqlite3")
}

async fn initialize_ledger(
    opts: &CliOptions,
    wallet_address: &str,
    chain_store: &SqliteChainStore,
    advertised_p2p_addr: SocketAddr,
    local_testnet: bool,
) -> Result<InitializedLedger> {
    if let Some(loaded) = chain_store.load_with_verification_status()? {
        let snapshot = loaded.snapshot;
        if opts.chain_mode == ChainMode::Genesis {
            bail!(
                "--genesis refuses to run because chain database already contains a blockchain at {}; start without --genesis to resume it",
                chain_store.path().display()
            );
        }
        let expected_profile = if local_testnet {
            LaunchProfile::local_testnet()
        } else {
            LaunchProfile::default()
        };
        if snapshot.launch_profile.profile_id != expected_profile.profile_id {
            return Ok(InitializedLedger {
                ledger: setup_ledger(local_testnet),
                migration_from: Some(snapshot.launch_profile.profile_id),
            });
        }
        let height = snapshot_height(&snapshot);
        match loaded.revalidation_from_height {
            Some(from_height) => println!(
                "validating local chain from height {from_height} for the current consensus ruleset..."
            ),
            None => println!(
                "local chain is trusted under the current consensus ruleset; skipping historical validation"
            ),
        }
        let ledger = Ledger::from_persisted_snapshot_revalidating_from(
            snapshot,
            loaded.revalidation_from_height,
        )
        .with_context(|| {
            format!(
                "failed to load chain database {}",
                chain_store.path().display()
            )
        })?;
        println!(
            "resumed chain from {} at height {height}",
            chain_store.path().display()
        );
        Ok(InitializedLedger {
            ledger,
            migration_from: None,
        })
    } else {
        let ledger = match opts.chain_mode {
            ChainMode::Setup => Ok(setup_ledger(local_testnet)),
            ChainMode::Genesis => start_genesis_ledger(wallet_address, local_testnet),
            ChainMode::Join => join_chain_ledger(&opts.join_peers, advertised_p2p_addr).await,
        }?;
        Ok(InitializedLedger {
            ledger,
            migration_from: None,
        })
    }
}

#[derive(Debug)]
struct InitializedLedger {
    ledger: Ledger,
    migration_from: Option<String>,
}

impl std::ops::Deref for InitializedLedger {
    type Target = Ledger;

    fn deref(&self) -> &Self::Target {
        &self.ledger
    }
}

impl std::ops::DerefMut for InitializedLedger {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ledger
    }
}

fn snapshot_height(snapshot: &ChainSnapshot) -> u64 {
    snapshot
        .blocks
        .last()
        .map(|block| block.height)
        .unwrap_or(0)
}

fn setup_ledger(local_testnet: bool) -> Ledger {
    if local_testnet {
        Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::new(),
            Vec::new(),
            1,
            LaunchProfile::local_testnet(),
        )
        .expect("empty local-testnet setup ledger is valid")
    } else {
        Ledger::new(BTreeMap::new(), 1)
    }
}

fn start_genesis_ledger(wallet_address: &str, local_testnet: bool) -> Result<Ledger> {
    let vdf_rounds = measure_initial_vdf_rounds();
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet_address.to_string(), GENESIS_BOOTSTRAP_BALANCE);
    let launch_profile = if local_testnet {
        LaunchProfile::local_testnet()
    } else {
        LaunchProfile::default()
    };
    Ledger::new_with_genesis_burns_and_profile(
        genesis,
        vec![GenesisBurn::new(
            wallet_address,
            GENESIS_BOOTSTRAP_BURN_AMOUNT,
        )],
        vdf_rounds,
        launch_profile,
    )
}

fn measure_initial_vdf_rounds() -> u64 {
    let seed = "iuna-vdf-calibration";
    let (measured_rounds, elapsed) = measure_vdf_rounds(
        seed,
        VDF_MEASUREMENT_INITIAL_ROUNDS,
        VDF_MEASUREMENT_MIN_ELAPSED,
        VDF_MEASUREMENT_MAX_ROUNDS,
    );
    let rounds = extrapolate_vdf_rounds(
        measured_rounds,
        elapsed,
        Duration::from_millis(VDF_TARGET_BLOCK_MS),
    );
    println!(
        "measured {measured_rounds} VDF rounds in {:.3}ms; initial VDF rounds: {rounds}",
        elapsed.as_secs_f64() * 1000.0
    );
    rounds
}

fn measure_vdf_rounds(
    seed: &str,
    initial_rounds: u64,
    min_elapsed: Duration,
    max_rounds_per_attempt: u64,
) -> (u64, Duration) {
    let mut rounds = initial_rounds.max(1).min(max_rounds_per_attempt.max(1));
    let mut measured_rounds = 0_u64;
    let mut measured_elapsed = Duration::ZERO;

    loop {
        let started = Instant::now();
        let _ = run_vdf(seed, rounds);
        measured_elapsed += started.elapsed();
        measured_rounds = measured_rounds.saturating_add(rounds);

        if measured_elapsed >= min_elapsed || rounds >= max_rounds_per_attempt {
            return (measured_rounds, measured_elapsed);
        }
        rounds = rounds.saturating_mul(2).min(max_rounds_per_attempt);
    }
}

fn extrapolate_vdf_rounds(measured_rounds: u64, elapsed: Duration, target: Duration) -> u64 {
    let elapsed_ns = elapsed.as_nanos().max(1);
    let target_ns = target.as_nanos().max(1);
    let rounds = u128::from(measured_rounds)
        .saturating_mul(target_ns)
        .saturating_div(elapsed_ns)
        .max(1);
    rounds.min(u128::from(MAX_VDF_ROUNDS)) as u64
}

async fn join_chain_ledger(join_peers: &[String], advertised_addr: SocketAddr) -> Result<Ledger> {
    let mut errors = Vec::new();
    for peer in join_peers {
        match p2p::fetch_snapshot_with_announcement(peer, Some(advertised_addr)).await {
            Ok(snapshot) => {
                let height = snapshot
                    .blocks
                    .last()
                    .map(|block| block.height)
                    .unwrap_or(0);
                println!("joined chain from {peer} at height {height}");
                return Ledger::from_snapshot(snapshot);
            }
            Err(error) => {
                errors.push(format!("{peer}: {error:#}"));
            }
        }
    }

    bail!(
        "could not join any requested peer; refusing to start a separate chain: {}",
        errors.join("; ")
    )
}

async fn run_automatic_finalizer(node: SharedNode, gossip: p2p::GossipNetwork, debug: bool) {
    let mut last_logged_skip: Option<(u64, String)> = None;
    loop {
        if !node.lock().await.has_real_chain() {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        let (height, plan, outbox) = {
            let mut node = node.lock().await;
            let height = node.chain_height();
            let plan = node.prepare_automatic_finalization(now_ms());
            let outbox = node.drain_outbox();
            (height, plan, outbox)
        };

        if let Err(error) = gossip.broadcast(outbox).await {
            if debug {
                eprintln!("p2p broadcast failed after automatic burn: {error:#}");
            }
        }

        let Some(work) = plan.work else {
            if let Some(reason) = &plan.skipped_reason {
                if debug
                    && should_log_automatic_finalization_skip(&mut last_logged_skip, height, reason)
                {
                    println!("auto-finalization skipped at height {height}: {reason}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };

        let candidate_height = work.height();
        let candidate_parent = work.prev_hash().to_string();
        let seed = work.vdf_seed().to_string();
        let rounds = work.vdf_rounds();
        let publish_at_ms = work.timestamp_ms();
        let precheck = {
            let node = node.lock().await;
            node.precheck_prepared_block_without_vdf_at(&work, now_ms())
        };
        if let Err(error) = precheck {
            let message = format!("skipped before VDF: {error:#}");
            if debug
                && should_log_automatic_finalization_skip(&mut last_logged_skip, height, &message)
            {
                println!("auto-finalization {message}");
            }
            node.lock()
                .await
                .record_automatic_finalization_status(message);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        last_logged_skip = None;
        if debug {
            println!(
                "leader selected locally for candidate block {}; running VDF for {} rounds",
                work.height(),
                work.vdf_rounds()
            );
        }
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let mut vdf_worker = tokio::task::spawn_blocking(move || {
            run_vdf_cancellable_with_progress(
                &seed,
                rounds,
                VDF_PROGRESS_LOG_INTERVAL,
                worker_cancellation.as_ref(),
                |progress| {
                    let _ = progress_tx.send(progress);
                },
            )
        });
        let mut cancelled_for_new_tip = false;
        let vdf_output = loop {
            tokio::select! {
                result = &mut vdf_worker => {
                    break match result {
                        Ok(output) => output,
                        Err(error) => {
                            if debug {
                                eprintln!("VDF worker failed: {error:#}");
                            }
                            None
                        }
                    };
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    while let Ok(progress) = progress_rx.try_recv() {
                        let message = format_vdf_progress(candidate_height, progress);
                        if debug {
                            println!("{message}");
                        }
                        node.lock().await.record_automatic_finalization_status(message);
                    }
                    let tip_changed = node.lock().await.ledger().tip_hash() != candidate_parent;
                    if tip_changed {
                        cancelled_for_new_tip = true;
                        cancellation.store(true, Ordering::Relaxed);
                    }
                }
            }
        };
        let Some(vdf_output) = vdf_output else {
            let message = if cancelled_for_new_tip {
                format!("cancelled stale VDF for candidate block {candidate_height}")
            } else {
                format!("VDF worker failed for candidate block {candidate_height}")
            };
            if debug {
                println!("auto-finalization {message}");
            }
            node.lock()
                .await
                .record_automatic_finalization_status(message);
            continue;
        };

        let completed_at_ms = now_ms();
        let publish_timestamp_ms = completed_at_ms.max(publish_at_ms);
        if completed_at_ms < publish_at_ms {
            let wait_ms = publish_at_ms - completed_at_ms;
            if debug {
                println!(
                    "VDF completed early for candidate block {}; waiting {:.3}s for rank time slot",
                    work.height(),
                    wait_ms as f64 / 1000.0
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        }

        let (finalized, outbox) = {
            let mut node = node.lock().await;
            let finalized = node.complete_prepared_block_at(work, vdf_output, publish_timestamp_ms);
            match &finalized {
                Ok(block) => node.record_automatic_finalization_status(format!(
                    "finalized block {} ({})",
                    block.height, block.hash
                )),
                Err(error) => node
                    .record_automatic_finalization_status(format!("skipped after VDF: {error:#}")),
            }
            let outbox = node.drain_outbox();
            (finalized, outbox)
        };

        let failed_after_vdf = finalized.is_err();
        match finalized {
            Ok(block) if debug => {
                println!("auto-finalized block {} ({})", block.height, block.hash);
            }
            Ok(_) => {}
            Err(error) if debug => println!("auto-finalization skipped after VDF: {error:#}"),
            Err(_) => {}
        }

        if let Err(error) = gossip.broadcast(outbox).await {
            if debug {
                eprintln!("p2p broadcast failed after automatic block: {error:#}");
            }
        }

        if failed_after_vdf {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }

        tokio::task::yield_now().await;
    }
}

fn should_log_automatic_finalization_skip(
    last_logged_skip: &mut Option<(u64, String)>,
    height: u64,
    reason: &str,
) -> bool {
    let skip = (height, reason.to_string());
    if last_logged_skip.as_ref() == Some(&skip) {
        return false;
    }
    *last_logged_skip = Some(skip);
    true
}

fn format_vdf_progress(candidate_height: u64, progress: VdfProgress) -> String {
    let phase = match progress.phase {
        VdfProgressPhase::Output => "output",
        VdfProgressPhase::Proof => "proof",
    };
    let percent = if progress.total_steps == 0 {
        100.0
    } else {
        progress.completed_steps as f64 * 100.0 / progress.total_steps as f64
    };
    format!(
        "running VDF for candidate block {candidate_height}: {phase} {}/{} rounds, total {}/{} steps ({percent:.1}%)",
        progress.completed_phase_rounds,
        progress.phase_rounds,
        progress.completed_steps,
        progress.total_steps
    )
}

async fn run_automatic_pow_miner(node: SharedNode, gossip: p2p::GossipNetwork, debug: bool) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let (height, job) = {
            let mut node = node.lock().await;
            if !node.pow_mining_enabled() {
                continue;
            }
            if !node.has_real_chain() {
                continue;
            }
            let height = node.chain_height();
            let job = match node.prepare_automatic_pow_mining_job() {
                Ok(job) => job,
                Err(error) => {
                    node.record_automatic_pow_mining_error(format!(
                        "automatic PoW mining failed: {error:#}"
                    ));
                    None
                }
            };
            (height, job)
        };
        let Some(job) = job else {
            continue;
        };

        let search = tokio::task::spawn_blocking(move || job.search()).await;
        let (pow_mined, outbox) = {
            let mut node = node.lock().await;
            let pow_mined = match search {
                Ok(Ok((job, outcome))) => {
                    match node.finish_automatic_pow_mining_job(job, outcome) {
                        Ok(tx) => tx,
                        Err(error) => {
                            node.record_automatic_pow_mining_error(format!(
                                "automatic PoW mining failed: {error:#}"
                            ));
                            None
                        }
                    }
                }
                Ok(Err(error)) => {
                    node.record_automatic_pow_mining_error(format!(
                        "automatic PoW mining failed: {error:#}"
                    ));
                    None
                }
                Err(error) => {
                    node.record_automatic_pow_mining_error(format!(
                        "automatic PoW mining task failed: {error:#}"
                    ));
                    None
                }
            };
            let outbox = node.drain_outbox();
            (pow_mined, outbox)
        };

        if let Err(error) = gossip.broadcast(outbox).await {
            if debug {
                eprintln!("p2p broadcast failed after automatic PoW mining: {error:#}");
            }
        }

        if debug {
            if let Some(tx) = &pow_mined {
                println!(
                    "auto-pow queued mine action for height {} ({})",
                    height,
                    tx.signature()
                );
            }
        }
    }
}

async fn run_peer_sync(node: SharedNode, gossip: p2p::GossipNetwork, debug: bool) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let envelopes = {
            let mut node = node.lock().await;
            let mut envelopes = vec![node.peer_status()];
            envelopes.extend(node.drain_outbox());
            envelopes.extend(node.mempool_gossip());
            envelopes
        };
        let mut envelopes = envelopes;
        envelopes.push(gossip.peer_exchange().await);
        if let Err(error) = gossip.broadcast(envelopes).await {
            if debug {
                eprintln!("p2p sync gossip failed: {error:#}");
            }
        }
    }
}

async fn run_chain_persistence(
    node: SharedNode,
    store: SqliteChainStore,
    ui_data_store: SqliteUiDataStore,
    ui_config: Arc<Mutex<config_store::UiConfig>>,
    gossip: p2p::GossipNetwork,
    initial_saved_tip: Option<String>,
    initial_projected_keep_metrics: bool,
) {
    let initial_state = ChainPersistenceState {
        projected_tip: initial_saved_tip.clone(),
        saved_tip: initial_saved_tip,
        projected_keep_metrics: initial_projected_keep_metrics,
        sync_checkpoint_interval: SYNC_CHAIN_CHECKPOINT_INTERVAL,
    };
    run_chain_persistence_loop(
        node,
        store,
        ui_data_store,
        ui_config,
        Duration::from_secs(2),
        Some(gossip),
        initial_state,
    )
    .await;
}

#[cfg(test)]
async fn run_chain_persistence_with_interval(
    node: SharedNode,
    store: SqliteChainStore,
    ui_data_store: SqliteUiDataStore,
    ui_config: Arc<Mutex<config_store::UiConfig>>,
    interval: Duration,
    initial_saved_tip: Option<String>,
    initial_projected_keep_metrics: bool,
) {
    let initial_state = ChainPersistenceState {
        projected_tip: initial_saved_tip.clone(),
        saved_tip: initial_saved_tip,
        projected_keep_metrics: initial_projected_keep_metrics,
        sync_checkpoint_interval: SYNC_CHAIN_CHECKPOINT_INTERVAL,
    };
    run_chain_persistence_loop(
        node,
        store,
        ui_data_store,
        ui_config,
        interval,
        None,
        initial_state,
    )
    .await;
}

struct ChainPersistenceState {
    saved_tip: Option<String>,
    projected_tip: Option<String>,
    projected_keep_metrics: bool,
    sync_checkpoint_interval: Duration,
}

async fn run_chain_persistence_loop(
    node: SharedNode,
    store: SqliteChainStore,
    ui_data_store: SqliteUiDataStore,
    ui_config: Arc<Mutex<config_store::UiConfig>>,
    interval: Duration,
    gossip: Option<p2p::GossipNetwork>,
    initial_state: ChainPersistenceState,
) {
    let mut last_saved_tip = initial_state.saved_tip;
    let mut last_projected_tip = initial_state.projected_tip;
    let mut last_projected_keep_metrics = initial_state.projected_keep_metrics;
    let mut last_chain_checkpoint = Instant::now();
    loop {
        tokio::time::sleep(interval).await;
        let syncing = gossip
            .as_ref()
            .is_some_and(|network| network.chain_sync_active_or_recent(Duration::from_secs(5)));
        let defer_sync_checkpoint = should_defer_sync_checkpoint(
            syncing,
            last_chain_checkpoint.elapsed(),
            initial_state.sync_checkpoint_interval,
        );
        let snapshot = {
            let node = node.lock().await;
            if !node.has_real_chain() {
                continue;
            }
            let tip_hash = node.ledger().tip_hash();
            if syncing {
                let tip_changed = last_saved_tip.as_deref() != Some(tip_hash);
                if !tip_changed || defer_sync_checkpoint {
                    continue;
                }
            }
            node.chain_snapshot()
        };
        let Some(tip_hash) = snapshot.blocks.last().map(|block| block.hash.clone()) else {
            continue;
        };
        let keep_metrics = ui_config.lock().await.keep_track_of_metrics;
        let tip_changed = last_saved_tip.as_deref() != Some(tip_hash.as_str());
        let projected_tip_changed = last_projected_tip.as_deref() != Some(tip_hash.as_str());
        let metrics_mode_changed = last_projected_keep_metrics != keep_metrics;
        if !tip_changed && !projected_tip_changed && !metrics_mode_changed {
            continue;
        }

        let result = if syncing && tip_changed {
            persist_chain_snapshot(&store, snapshot).await
        } else if tip_changed {
            persist_chain_and_project_ui_data(&store, &ui_data_store, snapshot, keep_metrics).await
        } else {
            project_ui_data_store(&ui_data_store, snapshot, keep_metrics).await
        };
        match result {
            Ok(()) => {
                if !syncing {
                    last_projected_tip = Some(tip_hash.clone());
                    last_projected_keep_metrics = keep_metrics;
                }
                last_saved_tip = Some(tip_hash);
                if tip_changed {
                    last_chain_checkpoint = Instant::now();
                }
            }
            Err(error) if debug_logging_enabled() => {
                eprintln!("chain persistence failed: {error:#}")
            }
            Err(_) => {}
        }
    }
}

fn should_defer_sync_checkpoint(
    syncing: bool,
    since_last_checkpoint: Duration,
    checkpoint_interval: Duration,
) -> bool {
    syncing && since_last_checkpoint < checkpoint_interval
}

async fn persist_chain_and_project_ui_data(
    store: &SqliteChainStore,
    ui_data_store: &SqliteUiDataStore,
    snapshot: ChainSnapshot,
    keep_metrics: bool,
) -> Result<()> {
    persist_chain_snapshot(store, snapshot.clone()).await?;
    project_ui_data_store(ui_data_store, snapshot, keep_metrics).await
}

async fn persist_chain_snapshot(store: &SqliteChainStore, snapshot: ChainSnapshot) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save_verified(&snapshot))
        .await
        .context("chain persistence worker failed")??;
    Ok(())
}

async fn warm_ui_data_store(
    store: &SqliteUiDataStore,
    snapshot: ChainSnapshot,
    keep_metrics: bool,
) -> Result<()> {
    println!("warming UI data database...");
    let started = Instant::now();
    let tip_hash = snapshot
        .blocks
        .last()
        .map(|block| block.hash.clone())
        .context("cannot warm UI data database from an empty chain")?;
    let readiness_store = store.clone();
    let readiness_tip = tip_hash.clone();
    let (ui_ready, metrics_ready) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            readiness_store.is_projected_to(&readiness_tip)?,
            readiness_store.metrics_are_projected_to(&readiness_tip)?,
        ))
    })
    .await
    .context("UI data readiness worker failed")??;

    if !ui_ready {
        project_ui_data_store(store, snapshot, keep_metrics).await?;
    } else if keep_metrics && !metrics_ready {
        let metrics_store = store.clone();
        tokio::task::spawn_blocking(move || metrics_store.replace_metrics_for_snapshot(&snapshot))
            .await
            .context("metrics warm-up worker failed")??;
    } else if !keep_metrics {
        let metrics_store = store.clone();
        tokio::task::spawn_blocking(move || metrics_store.clear_metrics())
            .await
            .context("metrics cleanup worker failed")??;
    }
    println!(
        "UI data database ready in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

async fn project_ui_data_store(
    store: &SqliteUiDataStore,
    snapshot: ChainSnapshot,
    keep_metrics: bool,
) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.project_snapshot(&snapshot, keep_metrics))
        .await
        .context("UI data projection worker failed")??;
    Ok(())
}

async fn clear_ui_data_store(store: &SqliteUiDataStore) -> Result<()> {
    println!("clearing UI data database...");
    let started = Instant::now();
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.clear_all())
        .await
        .context("UI data cleanup worker failed")??;
    println!(
        "UI data database ready in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
