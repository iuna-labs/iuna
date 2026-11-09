use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use getrandom::getrandom;
use iuna::{
    adapters::{
        consensus_http::{
            ConsensusHttpState, WalletState, operator_router, operator_token_hash,
            public_wallet_router,
        },
        consensus_local_state::{self, MempoolSnapshot},
        consensus_network::{
            ConsensusNetwork, ConsensusRelay, PeerAddress, SharedConsensusNode, seed_added_peers,
        },
        consensus_store::ConsensusChainStore,
        consensus_ui_store::ConsensusUiStore,
        consensus_wallet_store,
    },
    app::{consensus_node::Node, wallet_commands::TicketParameters},
    consensus::{LEDGER_TARGET_INTERVAL_HEARTBEATS, minimum_ticket_budget, subsidy},
};
use tokio::{net::TcpListener, sync::Mutex};

mod legacy_data;

#[derive(Debug, Eq, PartialEq)]
struct Options {
    data_dir: PathBuf,
    http_addr: SocketAddr,
    wallet_endpoint_addr: Option<SocketAddr>,
    p2p_addr: SocketAddr,
    peers: Vec<PeerAddress>,
    bootstrap_peers: Vec<PeerAddress>,
    no_bootstrap_peers: bool,
}

/// Public nodes a fresh node starts from, so it finds the network before anyone types an address.
const KNOWN_NODES: &str = include_str!("../KNOWN_NODES.txt");

fn known_nodes(text: &str) -> Result<Vec<PeerAddress>> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.parse().context("parsing KNOWN_NODES.txt"))
        .collect()
}

impl Default for Options {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            http_addr: "127.0.0.1:8080"
                .parse()
                .expect("valid default HTTP address"),
            wallet_endpoint_addr: None,
            p2p_addr: "0.0.0.0:9444".parse().expect("valid default P2P address"),
            peers: Vec::new(),
            bootstrap_peers: Vec::new(),
            no_bootstrap_peers: false,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(options) = parse_options(env::args().skip(1))? else {
        print_help();
        return Ok(());
    };
    std::fs::create_dir_all(&options.data_dir)
        .with_context(|| format!("creating data directory {}", options.data_dir.display()))?;
    for (source, backup) in legacy_data::archive(&options.data_dir)? {
        eprintln!(
            "legacy data: moved {} to {}",
            source.display(),
            backup.display()
        );
    }
    let chain_store = ConsensusChainStore::open(options.data_dir.join("chain.sqlite3"))?;
    let ui_store = ConsensusUiStore::open(options.data_dir.join("ui.sqlite3"))?;
    let wallet_path = options.data_dir.join("wallet.json");
    let node_instance_id = consensus_local_state::load_or_create_instance_id(&options.data_dir)?;
    let wallet_password = env::var("IUNA_WALLET_PASSWORD").ok();
    if wallet_password.as_deref() == Some("") {
        bail!("IUNA_WALLET_PASSWORD must not be empty");
    }
    let wallet = match (wallet_path.exists(), wallet_password.as_deref()) {
        (true, Some(password)) => match consensus_wallet_store::load(&wallet_path, password) {
            Ok(wallet) => Some(Arc::new(wallet)),
            Err(error) => {
                eprintln!("wallet remains locked: {error}");
                None
            }
        },
        // Initial Setup must show and verify the recovery phrase for new wallets.
        (false, Some(_)) => None,
        (_, None) => None,
    };
    let wallet_public_key = match &wallet {
        Some(wallet) => Some(wallet.public_key()),
        None if wallet_path.exists() => Some(consensus_wallet_store::public_key(&wallet_path)?),
        None => None,
    };
    let operator_token_hash = env::var("IUNA_OPERATOR_TOKEN")
        .ok()
        .map(|token| operator_token_hash(&token))
        .transpose()?;
    if operator_token_hash.is_some() && !options.http_addr.ip().is_loopback() {
        bail!("management API requires a loopback --http address");
    }
    let insecure_operator_api = env::var("IUNA_INSECURE_OPERATOR_API").as_deref() == Ok("1");
    let trusted_management = options.http_addr.ip().is_loopback() || insecure_operator_api;
    if insecure_operator_api {
        eprintln!("warning: unauthenticated operator API enabled for local development");
    }
    if let Some(wallet_addr) = options.wallet_endpoint_addr {
        if wallet_addr.port() == options.http_addr.port() {
            bail!("wallet endpoint port must differ from the operator HTTP port");
        }
        if wallet_addr.port() == options.p2p_addr.port() {
            bail!("wallet endpoint port must differ from the P2P listener port");
        }
    }
    let node = Arc::new(Mutex::new(Node::open(chain_store)?));
    let projected_tip = {
        let node = node.lock().await;
        ui_store.project(node.chain())?;
        node.status().tip
    };
    let mempool_path = consensus_local_state::mempool_path(&options.data_dir);
    match consensus_local_state::load_mempool(&mempool_path) {
        Ok(saved)
            if !saved.transactions.is_empty()
                || !saved.tickets.is_empty()
                || !saved.candidates.is_empty() =>
        {
            let total = saved.transactions.len() + saved.tickets.len() + saved.candidates.len();
            let restored = node.lock().await.restore_mempool(
                saved.transactions,
                saved.tickets,
                saved.candidates,
            );
            println!("mempool: restored {restored} of {total} saved entries");
        }
        Ok(_) => {}
        Err(error) => eprintln!("saved mempool ignored: {error:#}"),
    }

    let peer_store = options.data_dir.join("peers.json");
    seed_added_peers(&peer_store, &options.bootstrap_peers)?;
    let network = ConsensusNetwork::start_with_peer_store(
        Arc::clone(&node),
        options.p2p_addr,
        options.peers,
        Some(peer_store),
    )
    .await?;
    let http_listener = TcpListener::bind(options.http_addr)
        .await
        .with_context(|| format!("binding operator HTTP listener on {}", options.http_addr))?;
    let http_addr = http_listener.local_addr()?;
    let wallet_listener = match options.wallet_endpoint_addr {
        Some(addr) => Some(
            TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding public wallet listener on {addr}"))?,
        ),
        None => None,
    };
    println!("IUNA mainnet-candidate node");
    println!("operator API: http://{http_addr}/api/status");
    if operator_token_hash.is_some() {
        println!("management UI: http://{http_addr}/ (operator token)");
    } else if trusted_management {
        println!("management UI: http://{http_addr}/ (local browser, no token)");
    } else {
        println!("management UI: disabled");
    }
    println!("P2P listener: {}", network.local_addr());
    if let Some(listener) = &wallet_listener {
        println!("public wallet endpoint: http://{}", listener.local_addr()?);
    } else {
        println!("public wallet endpoint: disabled");
    }

    let projection_node = Arc::clone(&node);
    let projection_store = ui_store.clone();
    let projection = tokio::spawn(async move {
        let mut projected_tip = projected_tip;
        loop {
            let prepared = {
                let node = projection_node.lock().await;
                let tip = node.status().tip;
                (tip != projected_tip)
                    .then(|| (tip, projection_store.prepare_projection(node.chain())))
            };
            if let Some((tip, batch)) = prepared {
                // Only the build needs the node; the commit waits for the disk.
                let store = projection_store.clone();
                let result = tokio::task::spawn_blocking(move || store.apply_projection(&batch?))
                    .await
                    .context("joining screen-data projection")
                    .and_then(|result| result);
                if let Err(error) = result {
                    eprintln!("screen-data projection failed: {error:#}");
                } else {
                    projected_tip = tip;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });

    let relay = network.relay();
    let peers = network.peer_registry();
    let wallet_state = Arc::new(RwLock::new(WalletState {
        path: wallet_path,
        unlocked: wallet,
        public_key: wallet_public_key,
    }));
    // Resume mining after a restart when the operator left it on and the wallet is unlocked.
    let preference = consensus_local_state::load_mining(&consensus_local_state::mining_path(
        &wallet_state
            .read()
            .expect("wallet state lock poisoned")
            .path,
    ))
    .unwrap_or_else(|error| {
        eprintln!("saved mining preference ignored: {error:#}");
        None
    });
    let wallet_unlocked = wallet_state
        .read()
        .expect("wallet state lock poisoned")
        .unlocked
        .is_some();
    let heartbeat_mining = Arc::new(AtomicBool::new(
        preference.is_some_and(|preference| preference.enabled) && wallet_unlocked,
    ));
    let heartbeat_mining_workers = Arc::new(AtomicUsize::new(
        preference.map_or(1, |preference| preference.workers.clamp(1, 256)),
    ));
    let heartbeat_hashes = Arc::new(AtomicU64::new(0));
    let ledger_mining = Arc::new(AtomicBool::new(
        preference.is_some_and(|preference| preference.ledger_enabled) && wallet_unlocked,
    ));
    let ledger_last_ticket_height = Arc::new(AtomicU64::new(
        preference
            .and_then(|preference| preference.last_ledger_ticket_height)
            .and_then(|height| height.checked_add(1))
            .unwrap_or(0),
    ));
    let persistence = tokio::spawn(persist_mempool(Arc::clone(&node), mempool_path.clone()));
    let mining = tokio::spawn(run_heartbeat_miner(
        Arc::clone(&node),
        Arc::clone(&wallet_state),
        Arc::clone(&heartbeat_mining),
        Arc::clone(&heartbeat_mining_workers),
        Arc::clone(&heartbeat_hashes),
        relay.clone(),
    ));
    let ledger_mining_task = tokio::spawn(run_ledger_miner(
        Arc::clone(&node),
        Arc::clone(&wallet_state),
        Arc::clone(&heartbeat_mining),
        Arc::clone(&heartbeat_mining_workers),
        Arc::clone(&ledger_mining),
        Arc::clone(&ledger_last_ticket_height),
        relay.clone(),
    ));
    let http_state = ConsensusHttpState {
        node_instance_id,
        node: Arc::clone(&node),
        ui_store,
        wallet: wallet_state,
        operator_token_hash,
        trusted_management,
        relay: Some(relay),
        peers,
        heartbeat_mining,
        heartbeat_mining_workers,
        heartbeat_hashes,
        ledger_mining,
        ledger_last_ticket_height,
    };
    let application = operator_router(http_state.clone(), options.http_addr);
    let server = axum::serve(http_listener, application).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let wallet_server = async move {
        match wallet_listener {
            Some(listener) => axum::serve(listener, public_wallet_router(http_state))
                .await
                .context("serving public wallet API"),
            None => std::future::pending::<Result<()>>().await,
        }
    };
    let result = tokio::select! {
        result = server => result.context("serving operator API"),
        result = wallet_server => result,
    };
    projection.abort();
    let _ = projection.await;
    mining.abort();
    let _ = mining.await;
    ledger_mining_task.abort();
    let _ = ledger_mining_task.await;
    persistence.abort();
    let _ = persistence.await;
    if let Err(error) = save_mempool(&node, &mempool_path).await {
        eprintln!("saving mempool on shutdown failed: {error:#}");
    }
    network.shutdown().await;
    result
}

/// Writes the mempool to disk whenever it changed, at most every few seconds, so pending
/// transactions and tickets survive a restart.
async fn persist_mempool(node: SharedConsensusNode, path: PathBuf) {
    let mut saved_generation = None;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let generation = node.lock().await.mempool_generation();
        if saved_generation == Some(generation) {
            continue;
        }
        match save_mempool(&node, &path).await {
            Ok(()) => saved_generation = Some(generation),
            Err(error) => eprintln!("saving mempool failed: {error:#}"),
        }
    }
}

async fn save_mempool(node: &SharedConsensusNode, path: &std::path::Path) -> Result<()> {
    let snapshot = {
        let node = node.lock().await;
        MempoolSnapshot {
            transactions: node.pending_transactions(),
            tickets: node.pending_tickets(),
            candidates: node.pending_candidates(),
        }
    };
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || consensus_local_state::save_mempool(&path, &snapshot))
        .await
        .context("mempool save task failed")?
}

/// Rebuild the work template this often so its timestamp and Ledger contents stay current.
const MINING_TEMPLATE_REFRESH: Duration = Duration::from_secs(10);

/// Stops the blocking nonce search when the miner task is aborted, e.g. on shutdown.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn run_heartbeat_miner(
    node: SharedConsensusNode,
    wallet: Arc<RwLock<WalletState>>,
    enabled: Arc<AtomicBool>,
    workers: Arc<AtomicUsize>,
    hashes: Arc<AtomicU64>,
    relay: ConsensusRelay,
) {
    loop {
        if !enabled.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        let Some(wallet) = wallet
            .read()
            .expect("wallet state lock poisoned")
            .unlocked
            .clone()
        else {
            enabled.store(false, Ordering::Relaxed);
            continue;
        };
        // Heartbeat pacing comes only from proof of work and the ASERT target; the miner never
        // waits for the 60-second interval itself.
        let (parent_tip, prepared, timestamp) = {
            let node = node.lock().await;
            let prepared = node
                .prepare_next()
                .and_then(|prepared| Ok((prepared.timestamp(unix_time_secs())?, prepared)));
            match prepared {
                Ok((timestamp, prepared)) => (node.status().tip, prepared, timestamp),
                Err(error) => {
                    eprintln!("preparing Heartbeat mining work failed: {error:#}");
                    drop(node);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        };
        let public_key = wallet.public_key();
        let worker_count = workers.load(Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let search_stop = Arc::clone(&stop);
        let search_hashes = Arc::clone(&hashes);
        let _stop_on_drop = StopOnDrop(Arc::clone(&stop));
        let mut search = tokio::task::spawn_blocking(move || {
            prepared.mine_until_counted(
                timestamp,
                public_key,
                worker_count,
                &search_stop,
                &search_hashes,
            )
        });
        let refresh_at = tokio::time::Instant::now() + MINING_TEMPLATE_REFRESH;
        let mined = loop {
            tokio::select! {
                result = &mut search => break result,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    let stale = !enabled.load(Ordering::Relaxed)
                        || tokio::time::Instant::now() >= refresh_at
                        || node.lock().await.status().tip != parent_tip;
                    if stale {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            }
        };
        let block = match mined {
            Ok(Ok(Some(block))) => block,
            Ok(Ok(None)) => continue,
            Ok(Err(error)) => {
                eprintln!("Heartbeat proof-of-work search failed: {error:#}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(error) => {
                eprintln!("Heartbeat proof-of-work task failed: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let activated = node
            .lock()
            .await
            .submit_block(block.clone(), unix_time_secs());
        match activated {
            Ok(true) => relay.heartbeat(&block),
            Ok(false) => {}
            Err(error) => eprintln!("submitting mined Heartbeat failed: {error:#}"),
        }
    }
}

async fn run_ledger_miner(
    node: SharedConsensusNode,
    wallet_state: Arc<RwLock<WalletState>>,
    heartbeat_enabled: Arc<AtomicBool>,
    heartbeat_workers: Arc<AtomicUsize>,
    enabled: Arc<AtomicBool>,
    last_ticket_height: Arc<AtomicU64>,
    relay: ConsensusRelay,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (wallet, preference_path) = {
            let state = wallet_state.read().expect("wallet state lock poisoned");
            let Some(wallet) = state.unlocked.clone() else {
                enabled.store(false, Ordering::Relaxed);
                continue;
            };
            (wallet, consensus_local_state::mining_path(&state.path))
        };
        let signed_candidates = {
            let mut node = node.lock().await;
            match node.candidate_drafts(wallet.public_key()) {
                Ok(drafts) => {
                    let mut accepted = Vec::new();
                    for draft in drafts {
                        let candidate = match wallet.sign_candidate(draft) {
                            Ok(candidate) => candidate,
                            Err(error) => {
                                eprintln!("signing Ledger candidate failed: {error:#}");
                                continue;
                            }
                        };
                        match node.submit_candidate(candidate.clone()) {
                            Ok(true) => accepted.push(candidate),
                            Ok(false) => {}
                            Err(error) => {
                                eprintln!("submitting signed Ledger candidate failed: {error:#}")
                            }
                        }
                    }
                    accepted
                }
                Err(error) => {
                    eprintln!("building signed Ledger candidate failed: {error:#}");
                    Vec::new()
                }
            }
        };
        if !signed_candidates.is_empty() {
            relay.candidates(signed_candidates);
        }
        if !enabled.load(Ordering::Relaxed) {
            continue;
        }
        let mut lottery_seed = [0; 32];
        if getrandom(&mut lottery_seed).is_err() {
            eprintln!("continuous Ledger mining needs secure randomness");
            continue;
        }
        let submission = {
            let mut node = node.lock().await;
            let height = node.status().height;
            let encoded_last = last_ticket_height.load(Ordering::Relaxed);
            let due = encoded_last == 0
                || height
                    >= encoded_last
                        .saturating_sub(1)
                        .saturating_add(LEDGER_TARGET_INTERVAL_HEARTBEATS);
            let already_pending = node
                .pending_tickets()
                .iter()
                .any(|ticket| ticket.mining_public_key == wallet.public_key());
            if !due || already_pending {
                None
            } else {
                let budget = match recommended_ledger_budget(node.chain().tip()) {
                    Ok(budget) => budget,
                    Err(error) => {
                        eprintln!("calculating continuous Ledger budget failed: {error:#}");
                        continue;
                    }
                };
                match wallet.submit_consolidating_ticket(
                    &mut node,
                    TicketParameters {
                        budget,
                        duration: LEDGER_TARGET_INTERVAL_HEARTBEATS,
                        fee: 1,
                        mining_public_key: wallet.public_key(),
                        lottery_seed,
                    },
                ) {
                    Ok(receipt) => node
                        .pending_ticket(&receipt.id)
                        .cloned()
                        .map(|ticket| (height, ticket)),
                    Err(error) => {
                        eprintln!("continuous Ledger ticket skipped: {error:#}");
                        None
                    }
                }
            }
        };
        let Some((height, ticket)) = submission else {
            continue;
        };
        last_ticket_height.store(height.saturating_add(1), Ordering::Relaxed);
        relay.tickets(vec![ticket]);
        if let Err(error) = consensus_local_state::save_mining_from(&preference_path, || {
            consensus_local_state::MiningPreference {
                enabled: heartbeat_enabled.load(Ordering::Relaxed),
                workers: heartbeat_workers.load(Ordering::Relaxed),
                ledger_enabled: enabled.load(Ordering::Relaxed),
                last_ledger_ticket_height: Some(height),
            }
        }) {
            eprintln!("saving continuous Ledger mining state failed: {error:#}");
        }
    }
}

fn recommended_ledger_budget(state: &iuna::consensus::ConsensusState) -> Result<u64> {
    let current_reward = subsidy(state.ledger_blocks, state.minted)?;
    let current = minimum_ticket_budget(LEDGER_TARGET_INTERVAL_HEARTBEATS, current_reward)?;
    let after_ledger_blocks = state
        .ledger_blocks
        .checked_add(1)
        .context("Ledger block count overflow")?;
    let after_minted = state
        .minted
        .checked_add(current_reward)
        .context("minted supply overflow")?;
    let reward_after_same_heartbeat = subsidy(after_ledger_blocks, after_minted)?;
    let after = minimum_ticket_budget(
        LEDGER_TARGET_INTERVAL_HEARTBEATS,
        reward_after_same_heartbeat,
    )?;
    Ok(current.max(after))
}

fn parse_options(arguments: impl IntoIterator<Item = String>) -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "--data-dir" => {
                options.data_dir = PathBuf::from(value(&mut arguments, "--data-dir")?);
            }
            "--http" => {
                options.http_addr = value(&mut arguments, "--http")?
                    .parse()
                    .context("parsing --http socket address")?;
            }
            "--wallet-endpoint" => {
                options.wallet_endpoint_addr = Some(
                    value(&mut arguments, "--wallet-endpoint")?
                        .parse()
                        .context("parsing --wallet-endpoint socket address")?,
                );
            }
            "--p2p" => {
                options.p2p_addr = value(&mut arguments, "--p2p")?
                    .parse()
                    .context("parsing --p2p socket address")?;
            }
            "--bootstrap-peer" => {
                options.bootstrap_peers.push(
                    value(&mut arguments, "--bootstrap-peer")?
                        .parse()
                        .context("parsing --bootstrap-peer address")?,
                );
            }
            "--no-bootstrap-peers" => options.no_bootstrap_peers = true,
            "--peer" => {
                options.peers.push(
                    value(&mut arguments, "--peer")?
                        .parse()
                        .context("parsing --peer socket address")?,
                );
            }
            _ => bail!("unknown option {argument}; use --help"),
        }
    }
    if options.no_bootstrap_peers {
        options.bootstrap_peers.clear();
    } else if options.bootstrap_peers.is_empty() {
        options.bootstrap_peers = known_nodes(KNOWN_NODES)?;
    }
    Ok(Some(options))
}

fn value(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    arguments
        .next()
        .with_context(|| format!("{option} requires a value"))
}

fn default_data_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|home| !home.is_empty()))
        .map(PathBuf::from)
        .map(|home| home.join(".iuna"))
        .unwrap_or_else(|| PathBuf::from(".iuna"))
}

fn print_help() {
    println!(
        "IUNA mainnet-candidate node\n\n\
         Usage: iuna [options]\n\n\
         --data-dir PATH   Chain and screen-data directory (default ~/.iuna)\n\
         --http ADDRESS    Operator HTTP address (default 127.0.0.1:8080)\n\
         --wallet-endpoint ADDRESS  Opt-in public wallet API on a separate listener\n\
         --p2p ADDRESS     P2P listen address (default 0.0.0.0:9444)\n\
         --peer ADDRESS    Outbound peer, IP:port or host:port; may be repeated\n\
         --bootstrap-peer ADDRESS  Outbound peer added once to peers.json; may be repeated\n\
                           (default: the nodes in KNOWN_NODES.txt)\n\
         --no-bootstrap-peers  Start without bootstrap peers, as for a private network\n\
         -h, --help        Show this help\n\
\n\
         Environment:\n\
         IUNA_WALLET_PASSWORD  Unlock an existing encrypted wallet\n\
         IUNA_OPERATOR_TOKEN   Enable management API (minimum 32 bytes)\n\
         IUNA_INSECURE_OPERATOR_API=1  Allow unauthenticated management (development only)"
    );
}

fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary normally links the production parameters, so check its pinned genesis here.
    /// The explicit `e2e` feature substitutes the fast test network for the live TCP test.
    #[cfg(not(feature = "e2e"))]
    #[test]
    fn production_genesis_is_pinned_at_the_proof_of_work_limit() {
        use iuna::consensus::{GENESIS_HASH, MAX_TARGET, pinned_genesis};

        let (genesis, state) = pinned_genesis().unwrap();
        assert_eq!(genesis.hash(), GENESIS_HASH);
        assert_eq!(genesis.header.target, MAX_TARGET);
        assert_eq!(state.tip_target, MAX_TARGET);
        // 26 leading zero bits: one ~60-second Heartbeat for a single ~1 MH/s CPU thread.
        assert_eq!(&MAX_TARGET[..4], &[0x00, 0x00, 0x00, 0x3f]);
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn e2e_genesis_uses_the_fast_proof_of_work_limit() {
        use iuna::consensus::{GENESIS_HASH, MAX_TARGET, pinned_genesis};

        let (genesis, state) = pinned_genesis().unwrap();
        assert_eq!(genesis.hash(), GENESIS_HASH);
        assert_eq!(genesis.header.target, MAX_TARGET);
        assert_eq!(state.tip_target, MAX_TARGET);
        assert_eq!(&MAX_TARGET[..2], &[0x00, 0x0f]);
    }

    #[test]
    fn options_parse_repeatable_peers_and_listener_addresses() {
        let options = parse_options(
            [
                "--data-dir",
                "state",
                "--http",
                "127.0.0.1:9000",
                "--p2p",
                "127.0.0.1:9001",
                "--wallet-endpoint",
                "127.0.0.1:9004",
                "--peer",
                "127.0.0.1:9002",
                "--peer",
                "127.0.0.1:9003",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap()
        .unwrap();

        assert_eq!(options.data_dir, PathBuf::from("state"));
        assert_eq!(options.http_addr.port(), 9000);
        assert_eq!(options.p2p_addr.port(), 9001);
        assert_eq!(options.wallet_endpoint_addr.unwrap().port(), 9004);
        assert_eq!(options.peers.len(), 2);
    }

    #[test]
    fn a_fresh_node_starts_from_the_known_nodes_unless_told_otherwise() {
        let parse = |arguments: &[&str]| {
            parse_options(arguments.iter().map(|argument| argument.to_string()))
                .unwrap()
                .unwrap()
                .bootstrap_peers
                .iter()
                .map(|peer| peer.to_string())
                .collect::<Vec<_>>()
        };

        assert!(parse(&[]).contains(&"iuna.jhx.app:9444".to_owned()));
        assert_eq!(
            parse(&["--bootstrap-peer", "203.0.113.7:9444"]),
            ["203.0.113.7:9444"]
        );
        assert!(parse(&["--no-bootstrap-peers"]).is_empty());
        assert_eq!(
            known_nodes("# comment\n\n  one.example:1  \n").unwrap(),
            ["one.example:1".parse::<PeerAddress>().unwrap()]
        );
    }

    #[test]
    fn missing_and_unknown_options_fail_closed() {
        assert!(parse_options(["--http".to_owned()]).is_err());
        assert!(parse_options(["--wallet-endpoint".to_owned()]).is_err());
        assert!(parse_options(["--mystery".to_owned()]).is_err());
    }

    #[test]
    fn operator_tokens_have_a_strict_minimum_length() {
        assert!(operator_token_hash("short").is_err());
        assert!(operator_token_hash("0123456789abcdef0123456789abcdef").is_ok());
    }
}
