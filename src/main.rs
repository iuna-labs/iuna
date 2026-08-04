use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use iuna::{
    adapters::{chain_store::SqliteChainStore, config_store, http, p2p, stratum, wallet_store},
    app::{
        NodeCore, PeerBook, SharedNode, SharedPeerBook, StratumStatus, debug_logging_enabled,
        now_ms, set_debug_logging,
    },
    domain::{
        Amount, ChainSnapshot, GenesisBurn, Ledger, MAX_VDF_ROUNDS, MICRO_IUNA,
        VDF_TARGET_BLOCK_MS, run_vdf,
    },
};
use tokio::sync::Mutex;

const GENESIS_BOOTSTRAP_BURN_AMOUNT: Amount = MICRO_IUNA;
const GENESIS_INITIAL_BURN_PER_BLOCK: Amount = GENESIS_BOOTSTRAP_BURN_AMOUNT;
const GENESIS_INITIAL_BURN_FEE: Amount = 0;
const VDF_MEASUREMENT_INITIAL_ROUNDS: u64 = 1_000_000;
const VDF_MEASUREMENT_MAX_ROUNDS: u64 = 100_000_000;
const VDF_MEASUREMENT_MIN_ELAPSED: Duration = Duration::from_millis(150);

#[tokio::main]
async fn main() -> Result<()> {
    let Some(opts) = CliOptions::parse()? else {
        return Ok(());
    };
    set_debug_logging(opts.debug);
    let debug_logging = opts.debug;
    let wallet_path = opts.wallet_path();
    let config_path = opts.config_path();
    let wallet_file_exists = wallet_path.exists();
    validate_wallet_for_mode(&opts, &wallet_path, wallet_file_exists)?;
    let chain_store = SqliteChainStore::open(opts.chain_db_path())?;
    let persisted_chain_exists = chain_store.load()?.is_some();
    if opts.chain_mode == ChainMode::Genesis && persisted_chain_exists {
        bail!(
            "--genesis refuses to run because chain database already contains a blockchain at {}; start without --genesis to resume it",
            chain_store.path().display()
        );
    }
    let mut ui_config = config_store::load_or_create(&config_path)?;
    let p2p_announce_addr = configured_p2p_announce_addr(&opts, &ui_config)?;
    let p2p_accept_inbound = opts.p2p_announce_addr.is_some() || ui_config.p2p_accept_inbound;
    if let Some(addr) = opts.p2p_announce_addr {
        ui_config.p2p_accept_inbound = true;
        ui_config.p2p_announce_addr = Some(addr.to_string());
    }
    let advertised_p2p_addr = p2p_announce_addr.unwrap_or(opts.p2p_addr);
    let wallet_load = load_startup_wallet(&wallet_path)?;
    let wallet_address = wallet_load.address().to_string();
    if opts.chain_mode == ChainMode::Genesis {
        ui_config.setup_complete = false;
        ui_config.mining_enabled = true;
        ui_config.pow_mining_enabled = false;
        ui_config.burn_per_block = GENESIS_INITIAL_BURN_PER_BLOCK;
        ui_config.burn_fee = GENESIS_INITIAL_BURN_FEE;
        config_store::save(&config_path, &ui_config)?;
    }
    let ledger =
        initialize_ledger(&opts, &wallet_address, &chain_store, advertised_p2p_addr).await?;
    let has_chain = opts.has_chain() || persisted_chain_exists;
    let initial_burn_per_block = initial_burn_per_block(&opts, &ui_config);
    let initial_burn_fee = initial_burn_fee(&opts, &ui_config);

    let mut node_core = match wallet_load {
        StartupWallet::Unlocked(wallet) => NodeCore::from_ledger_with_burn_fee_and_enabled(
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
    node_core.set_pow_mining_enabled(ui_config.pow_mining_enabled);
    let node: SharedNode = Arc::new(Mutex::new(node_core));
    let ui_config = Arc::new(Mutex::new(ui_config));
    let mut peers = ui_config.lock().await.peers.clone();
    peers.extend(opts.peers);
    let peers: SharedPeerBook = Arc::new(Mutex::new(PeerBook::from_addresses(peers)));
    if has_chain {
        let initial_snapshot = { node.lock().await.chain_snapshot() };
        persist_chain_snapshot(
            &chain_store,
            initial_snapshot,
            ui_config.lock().await.keep_track_of_metrics,
        )
        .await?;
    }

    println!("iuna wallet: {}", node.lock().await.wallet_address());
    if node.lock().await.wallet_is_locked() {
        println!("wallet locked: unlock it in the management UI");
    }
    println!("wallet file: {}", wallet_path.display());
    println!("config file: {}", config_path.display());
    println!("chain database: {}", chain_store.path().display());
    println!("management UI: http://{}", opts.http_addr);
    if p2p_accept_inbound {
        println!("p2p listener: {}", opts.p2p_addr);
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
        opts.p2p_addr,
        p2p_announce_addr,
        p2p_accept_inbound,
    )
    .await?;
    let mut stratum_status = StratumStatus {
        enabled: false,
        listen_addr: None,
    };
    if let Some(stratum_addr) = opts.stratum_addr {
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
    let persistence_config = Arc::clone(&ui_config);
    tokio::spawn(async move {
        run_chain_persistence(persistence_node, persistence_store, persistence_config).await;
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
            wallet_path,
            stratum: stratum_status,
            addr: opts.http_addr,
        },
    )
    .await
}

enum StartupWallet {
    Unlocked(iuna::domain::Wallet),
    Locked { address: String },
}

impl StartupWallet {
    fn address(&self) -> &str {
        match self {
            Self::Unlocked(wallet) => wallet.address(),
            Self::Locked { address } => address,
        }
    }
}

fn load_startup_wallet(wallet_path: &Path) -> Result<StartupWallet> {
    match wallet_store::load_or_create(wallet_path) {
        Ok(wallet) => Ok(StartupWallet::Unlocked(wallet)),
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

async fn initialize_ledger(
    opts: &CliOptions,
    wallet_address: &str,
    chain_store: &SqliteChainStore,
    advertised_p2p_addr: SocketAddr,
) -> Result<Ledger> {
    if let Some(snapshot) = chain_store.load()? {
        if opts.chain_mode == ChainMode::Genesis {
            bail!(
                "--genesis refuses to run because chain database already contains a blockchain at {}; start without --genesis to resume it",
                chain_store.path().display()
            );
        }
        let height = snapshot_height(&snapshot);
        let ledger = Ledger::from_persisted_snapshot(snapshot).with_context(|| {
            format!(
                "failed to load chain database {}",
                chain_store.path().display()
            )
        })?;
        println!(
            "resumed chain from {} at height {height}",
            chain_store.path().display()
        );
        Ok(ledger)
    } else {
        match opts.chain_mode {
            ChainMode::Setup => Ok(setup_ledger()),
            ChainMode::Genesis => start_genesis_ledger(wallet_address),
            ChainMode::Join => join_chain_ledger(&opts.join_peers, advertised_p2p_addr).await,
        }
    }
}

fn configured_p2p_announce_addr(
    opts: &CliOptions,
    ui_config: &config_store::UiConfig,
) -> Result<Option<SocketAddr>> {
    if let Some(addr) = opts.p2p_announce_addr {
        return Ok(Some(addr));
    }
    ui_config
        .p2p_announce_addr
        .as_deref()
        .map(|addr| {
            addr.parse()
                .with_context(|| format!("invalid configured P2P announce address {addr}"))
        })
        .transpose()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChainMode {
    Setup,
    Genesis,
    Join,
}

#[derive(Debug)]
struct CliOptions {
    wallet_path: Option<PathBuf>,
    chain_db_path: Option<PathBuf>,
    http_addr: SocketAddr,
    p2p_addr: SocketAddr,
    p2p_announce_addr: Option<SocketAddr>,
    stratum_addr: Option<SocketAddr>,
    peers: Vec<String>,
    join_peers: Vec<String>,
    chain_mode: ChainMode,
    data_dir: PathBuf,
    debug: bool,
}

impl CliOptions {
    fn parse() -> Result<Option<Self>> {
        Self::parse_from(std::env::args().skip(1))
    }

    fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Option<Self>> {
        let mut opts = Self {
            wallet_path: None,
            chain_db_path: None,
            http_addr: SocketAddr::from_str("127.0.0.1:18661")?,
            p2p_addr: SocketAddr::from_str("127.0.0.1:9444")?,
            p2p_announce_addr: None,
            stratum_addr: None,
            peers: Vec::new(),
            join_peers: Vec::new(),
            chain_mode: ChainMode::Setup,
            data_dir: default_data_dir(),
            debug: false,
        };

        let raw_args = args.into_iter().collect::<Vec<_>>();
        let mut args = raw_args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--genesis" => {
                    if opts.chain_mode == ChainMode::Join {
                        bail!("choose either --genesis or --join, not both");
                    }
                    opts.chain_mode = ChainMode::Genesis;
                }
                "--wallet" => {
                    opts.wallet_path = Some(PathBuf::from(next_value(&mut args, "--wallet")?))
                }
                "--chain-db" => {
                    opts.chain_db_path = Some(PathBuf::from(next_value(&mut args, "--chain-db")?))
                }
                "--wallet-seed" => {
                    bail!(
                        "--wallet-seed was removed; wallets are stored in --wallet <path> or ~/.iuna/wallet.json"
                    )
                }
                "--http" => {
                    opts.http_addr = next_value(&mut args, "--http")?
                        .parse()
                        .context("invalid --http address")?;
                }
                "--p2p" => {
                    opts.p2p_addr = next_value(&mut args, "--p2p")?
                        .parse()
                        .context("invalid --p2p address")?;
                }
                "--p2p-announce" => {
                    opts.p2p_announce_addr = Some(
                        next_value(&mut args, "--p2p-announce")?
                            .parse()
                            .context("invalid --p2p-announce address")?,
                    );
                }
                "--stratum" => {
                    opts.stratum_addr = Some(
                        next_value(&mut args, "--stratum")?
                            .parse()
                            .context("invalid --stratum address")?,
                    );
                }
                "--join" => {
                    if opts.chain_mode == ChainMode::Genesis {
                        bail!("choose either --genesis or --join, not both");
                    }
                    let peer = next_value(&mut args, "--join")?;
                    opts.chain_mode = ChainMode::Join;
                    opts.peers.push(peer.clone());
                    opts.join_peers.push(peer);
                }
                "--data-dir" => opts.data_dir = PathBuf::from(next_value(&mut args, "--data-dir")?),
                "--debug" => opts.debug = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}; pass --help for usage"),
            }
        }

        if opts.chain_mode == ChainMode::Genesis && !opts.join_peers.is_empty() {
            bail!("choose either --genesis or --join, not both");
        }

        Ok(Some(opts))
    }

    fn wallet_path(&self) -> PathBuf {
        self.wallet_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("wallet.json"))
    }

    fn chain_db_path(&self) -> PathBuf {
        self.chain_db_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("chain.sqlite3"))
    }

    fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    fn has_chain(&self) -> bool {
        self.chain_mode != ChainMode::Setup
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value after {flag}"))
}

fn validate_wallet_for_mode(
    opts: &CliOptions,
    wallet_path: &Path,
    wallet_file_exists: bool,
) -> Result<()> {
    if opts.chain_mode == ChainMode::Genesis && wallet_file_exists {
        bail!(
            "--genesis requires a fresh wallet path, but {} already exists; start without --genesis to reuse it or choose an empty --data-dir/--wallet",
            wallet_path.display()
        );
    }
    Ok(())
}

fn initial_burn_per_block(opts: &CliOptions, ui_config: &config_store::UiConfig) -> Amount {
    match opts.chain_mode {
        ChainMode::Genesis => GENESIS_INITIAL_BURN_PER_BLOCK,
        ChainMode::Setup | ChainMode::Join => ui_config.burn_per_block,
    }
}

fn initial_burn_fee(opts: &CliOptions, ui_config: &config_store::UiConfig) -> Amount {
    match opts.chain_mode {
        ChainMode::Genesis => GENESIS_INITIAL_BURN_FEE,
        ChainMode::Setup | ChainMode::Join => ui_config.burn_fee,
    }
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    "iuna\n\n\
         Usage:\n\
           iuna [options]\n\
           iuna --genesis [options]\n\
           iuna --join <addr:port> [options]\n\n\
         Options:\n\
           --genesis                     Create a new chain with a fresh setup wallet\n\
           --wallet <path>               Wallet file (default <data-dir>/wallet.json)\n\
           --chain-db <path>             Chain SQLite database (default <data-dir>/chain.sqlite3)\n\
           --http <addr:port>            HTTP management UI address (default 127.0.0.1:18661)\n\
           --p2p <addr:port>             Inbound P2P listener address when public node is enabled\n\
           --p2p-announce <addr:port>    Public P2P address to gossip; enables inbound P2P\n\
           --stratum <addr:port>         Stratum V1 listener for SHA-256 ASIC miners\n\
           --join <addr:port>            Fetch chain snapshot from this peer before finalization\n\
           --data-dir <path>             Local wallet directory (default ~/.iuna)\n\
           --debug                       Print verbose runtime logs\n\n\
         Environment:\n\
           IUNA_DEV_SKIP_SEED_VERIFY=1 Show a setup button to skip seed verification\n"
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()))
        .map(PathBuf::from)
        .map(|home| home.join(".iuna"))
        .unwrap_or_else(|| PathBuf::from(".iuna"))
}

fn snapshot_height(snapshot: &ChainSnapshot) -> u64 {
    snapshot
        .blocks
        .last()
        .map(|block| block.height)
        .unwrap_or(0)
}

fn setup_ledger() -> Ledger {
    Ledger::new(BTreeMap::new(), 1)
}

fn start_genesis_ledger(wallet_address: &str) -> Result<Ledger> {
    let vdf_rounds = measure_initial_vdf_rounds();
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet_address.to_string(), GENESIS_BOOTSTRAP_BURN_AMOUNT);
    Ledger::new_with_genesis_burns(
        genesis,
        vec![GenesisBurn::new(
            wallet_address,
            GENESIS_BOOTSTRAP_BURN_AMOUNT,
        )],
        vdf_rounds,
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
                let skip = (height, reason.clone());
                if debug && last_logged_skip.as_ref() != Some(&skip) {
                    println!("auto-finalization skipped at height {height}: {reason}");
                    last_logged_skip = Some(skip);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };

        last_logged_skip = None;
        if debug {
            println!(
                "leader selected locally for candidate block {}; running VDF for {} rounds",
                work.height(),
                work.vdf_rounds()
            );
        }

        let seed = work.vdf_seed().to_string();
        let rounds = work.vdf_rounds();
        let publish_at_ms = work.timestamp_ms();
        let vdf_output = match tokio::task::spawn_blocking(move || run_vdf(&seed, rounds)).await {
            Ok(output) => output,
            Err(error) => {
                if debug {
                    eprintln!("VDF worker failed: {error:#}");
                }
                continue;
            }
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
            let outbox = node.drain_outbox();
            (finalized, outbox)
        };

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

        tokio::task::yield_now().await;
    }
}

async fn run_automatic_pow_miner(node: SharedNode, gossip: p2p::GossipNetwork, debug: bool) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let (height, pow_mined, outbox) = {
            let mut node = node.lock().await;
            if !node.has_real_chain() {
                continue;
            }
            let height = node.chain_height();
            let pow_mined = match node.prepare_automatic_pow_mining() {
                Ok(tx) => tx,
                Err(error) => {
                    node.record_automatic_pow_mining_error(format!(
                        "automatic PoW mining failed: {error:#}"
                    ));
                    None
                }
            };
            let outbox = node.drain_outbox();
            (height, pow_mined, outbox)
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
    ui_config: Arc<Mutex<config_store::UiConfig>>,
) {
    run_chain_persistence_with_interval(node, store, ui_config, Duration::from_secs(2)).await;
}

async fn run_chain_persistence_with_interval(
    node: SharedNode,
    store: SqliteChainStore,
    ui_config: Arc<Mutex<config_store::UiConfig>>,
    interval: Duration,
) {
    let mut last_saved_tip: Option<String> = None;
    loop {
        tokio::time::sleep(interval).await;
        let snapshot = {
            let node = node.lock().await;
            if !node.has_real_chain() {
                continue;
            }
            node.chain_snapshot()
        };
        let Some(tip_hash) = snapshot.blocks.last().map(|block| block.hash.clone()) else {
            continue;
        };
        if last_saved_tip.as_deref() == Some(tip_hash.as_str()) {
            continue;
        }

        let keep_metrics = ui_config.lock().await.keep_track_of_metrics;
        match persist_chain_snapshot(&store, snapshot, keep_metrics).await {
            Ok(()) => last_saved_tip = Some(tip_hash),
            Err(error) if debug_logging_enabled() => {
                eprintln!("chain persistence failed: {error:#}")
            }
            Err(_) => {}
        }
    }
}

async fn persist_chain_snapshot(
    store: &SqliteChainStore,
    snapshot: ChainSnapshot,
    keep_metrics: bool,
) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save_with_metrics(&snapshot, keep_metrics))
        .await
        .context("chain persistence worker failed")??;
    Ok(())
}

#[cfg(test)]
mod tests {
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
        ChainMode, CliOptions, GENESIS_INITIAL_BURN_FEE, GENESIS_INITIAL_BURN_PER_BLOCK,
        StartupWallet, configured_p2p_announce_addr, extrapolate_vdf_rounds, help_text,
        initial_burn_fee, initial_burn_per_block, initialize_ledger, load_startup_wallet,
        measure_vdf_rounds, persist_chain_snapshot, run_chain_persistence_with_interval,
        validate_wallet_for_mode,
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
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap()
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
            StartupWallet::Unlocked(_) => panic!("encrypted wallet should start locked"),
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
    fn genesis_mode_starts_with_bootstrap_burn_rate_and_zero_fee() {
        let genesis = parse(&["--genesis"]).unwrap().unwrap();
        let configured = UiConfig {
            burn_per_block: 50,
            burn_fee: 3,
            ..UiConfig::default()
        };
        assert_eq!(
            initial_burn_per_block(&genesis, &configured),
            GENESIS_INITIAL_BURN_PER_BLOCK
        );
        assert_eq!(
            initial_burn_fee(&genesis, &configured),
            GENESIS_INITIAL_BURN_FEE
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

        assert_eq!(
            first.burned.as_ref().map(|tx| tx.amount()),
            Some(MICRO_IUNA)
        );
        assert!(first.block.is_some(), "{first:?}");
        assert_eq!(
            second.burned.as_ref().map(|tx| tx.amount()),
            Some(MICRO_IUNA)
        );
        assert!(second.block.is_some(), "{second:?}");
        assert!(
            second.skipped_reason.as_deref().is_none_or(|reason| {
                !reason.contains("block must include at least one burn transaction")
            }),
            "{second:?}"
        );
        assert!(node.ledger().balance_of(wallet.address()) >= BLOCK_REWARD - 2 * MICRO_IUNA);
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
}
