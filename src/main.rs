use std::{
    collections::BTreeMap, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc, time::Duration,
};

use anyhow::{Context, Result, bail};
use luun::{
    adapters::{chain_store::SqliteChainStore, config_store, http, p2p, wallet_store},
    app::{DEFAULT_VDF_ROUNDS, NodeCore, PeerBook, SharedNode, SharedPeerBook, now_ms},
    domain::{Amount, ChainSnapshot, Ledger, MICRO_LUUN, run_vdf},
};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    let Some(opts) = CliOptions::parse()? else {
        return Ok(());
    };
    let wallet_path = opts.wallet_path();
    let config_path = opts.config_path();
    let chain_store = SqliteChainStore::open(opts.chain_db_path())?;
    let persisted_chain_exists = chain_store.load()?.is_some();
    let wallet = wallet_store::load_or_create(&wallet_path)?;
    let ui_config = config_store::load_or_create(&config_path)?;
    let ledger = initialize_ledger(&opts, &chain_store).await?;
    let has_chain = ledger.is_started() || persisted_chain_exists;
    let initial_burn_per_block = initial_burn_per_block(&ui_config);
    let initial_burn_fee = initial_burn_fee(&ui_config);

    let node: SharedNode = Arc::new(Mutex::new(NodeCore::from_ledger_with_burn_fee(
        wallet,
        ledger,
        initial_burn_per_block,
        initial_burn_fee,
    )));
    let ui_config = Arc::new(Mutex::new(ui_config));
    let mut peers = ui_config.lock().await.peers.clone();
    peers.extend(opts.peers);
    let peers: SharedPeerBook = Arc::new(Mutex::new(PeerBook::from_addresses(peers)));
    if has_chain {
        let initial_snapshot = { node.lock().await.chain_snapshot() };
        if snapshot_chain_started(&initial_snapshot) {
            persist_chain_snapshot(&chain_store, initial_snapshot).await?;
        }
    }

    println!("luun wallet: {}", node.lock().await.wallet_address());
    println!("wallet file: {}", wallet_path.display());
    println!("config file: {}", config_path.display());
    println!("chain database: {}", chain_store.path().display());
    println!("management UI: http://{}", opts.http_addr);
    println!("p2p listener: {}", opts.p2p_addr);
    println!(
        "automatic mining: VDF-driven, burning {} LUUN per block with {} LUUN fee",
        format_luun(initial_burn_per_block),
        format_luun(initial_burn_fee)
    );

    let gossip =
        p2p::GossipNetwork::start(Arc::clone(&node), Arc::clone(&peers), opts.p2p_addr).await?;

    let persistence_node = Arc::clone(&node);
    let persistence_store = chain_store.clone();
    tokio::spawn(async move {
        run_chain_persistence(persistence_node, persistence_store).await;
    });

    let miner_node = Arc::clone(&node);
    let miner_gossip = gossip.clone();
    tokio::spawn(async move {
        run_automatic_miner(miner_node, miner_gossip).await;
    });

    let sync_node = Arc::clone(&node);
    let sync_gossip = gossip.clone();
    tokio::spawn(async move {
        run_peer_sync(sync_node, sync_gossip).await;
    });

    http::serve(
        node,
        peers,
        gossip,
        ui_config,
        config_path,
        wallet_path,
        opts.http_addr,
    )
    .await
}

fn format_luun(amount: Amount) -> String {
    let whole = amount / MICRO_LUUN;
    let fractional = amount % MICRO_LUUN;
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

async fn initialize_ledger(opts: &CliOptions, chain_store: &SqliteChainStore) -> Result<Ledger> {
    if let Some(snapshot) = chain_store.load()? {
        let height = snapshot_height(&snapshot);
        let ledger = Ledger::from_snapshot(snapshot).with_context(|| {
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
            ChainMode::Join => join_chain_ledger(&opts.join_peers, opts.p2p_addr).await,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChainMode {
    Setup,
    Join,
}

#[derive(Debug)]
struct CliOptions {
    wallet_path: Option<PathBuf>,
    chain_db_path: Option<PathBuf>,
    http_addr: SocketAddr,
    p2p_addr: SocketAddr,
    peers: Vec<String>,
    join_peers: Vec<String>,
    chain_mode: ChainMode,
    data_dir: PathBuf,
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
            peers: Vec::new(),
            join_peers: Vec::new(),
            chain_mode: ChainMode::Setup,
            data_dir: PathBuf::from(".luun"),
        };

        let raw_args = args.into_iter().collect::<Vec<_>>();
        let mut args = raw_args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--wallet" => {
                    opts.wallet_path = Some(PathBuf::from(next_value(&mut args, "--wallet")?))
                }
                "--chain-db" => {
                    opts.chain_db_path = Some(PathBuf::from(next_value(&mut args, "--chain-db")?))
                }
                "--wallet-seed" => {
                    bail!(
                        "--wallet-seed was removed; wallets are stored in --wallet <path> or .luun/wallet.json"
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
                "--join" => {
                    let peer = next_value(&mut args, "--join")?;
                    opts.chain_mode = ChainMode::Join;
                    opts.peers.push(peer.clone());
                    opts.join_peers.push(peer);
                }
                "--data-dir" => opts.data_dir = PathBuf::from(next_value(&mut args, "--data-dir")?),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}; pass --help for usage"),
            }
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
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value after {flag}"))
}

fn initial_burn_per_block(ui_config: &config_store::UiConfig) -> Amount {
    ui_config.burn_per_block
}

fn initial_burn_fee(ui_config: &config_store::UiConfig) -> Amount {
    ui_config.burn_fee
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    "luun\n\n\
         Usage:\n\
           luun [options]\n\
           luun --join <addr:port> [options]\n\n\
         Options:\n\
           --wallet <path>               Wallet file (default <data-dir>/wallet.json)\n\
           --chain-db <path>             Chain SQLite database (default <data-dir>/chain.sqlite3)\n\
           --http <addr:port>            HTTP management UI address (default 127.0.0.1:18661)\n\
           --p2p <addr:port>             P2P TCP listener address (default 127.0.0.1:9444)\n\
           --join <addr:port>            Fetch chain snapshot from this peer before mining\n\
           --data-dir <path>             Local wallet directory\n\n\
         Environment:\n\
           LUUN_DEV_SKIP_SEED_VERIFY=1 Show a setup button to skip seed verification\n"
}

fn snapshot_height(snapshot: &ChainSnapshot) -> u64 {
    snapshot
        .blocks
        .last()
        .map(|block| block.height)
        .unwrap_or(0)
}

fn snapshot_chain_started(snapshot: &ChainSnapshot) -> bool {
    snapshot.blocks.first().is_some_and(|genesis| {
        !snapshot.genesis_allocations.is_empty() || !genesis.transactions.is_empty()
    })
}

fn setup_ledger() -> Ledger {
    Ledger::new(BTreeMap::new(), DEFAULT_VDF_ROUNDS)
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

async fn run_automatic_miner(node: SharedNode, gossip: p2p::GossipNetwork) {
    let mut last_logged_skip: Option<(u64, String)> = None;
    loop {
        let (height, plan, outbox) = {
            let mut node = node.lock().await;
            let height = node.chain_height();
            let plan = node.prepare_automatic_mining(now_ms());
            let outbox = node.drain_outbox();
            (height, plan, outbox)
        };

        if let Err(error) = gossip.broadcast(outbox).await {
            eprintln!("p2p broadcast failed after automatic burn: {error:#}");
        }

        let Some(work) = plan.work else {
            if let Some(reason) = &plan.skipped_reason {
                let skip = (height, reason.clone());
                if last_logged_skip.as_ref() != Some(&skip) {
                    println!("auto-mining skipped at height {height}: {reason}");
                    last_logged_skip = Some(skip);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };

        last_logged_skip = None;
        println!(
            "leader selected locally for candidate block {}; running VDF for {} rounds",
            work.height(),
            work.vdf_rounds()
        );

        let seed = work.vdf_seed().to_string();
        let rounds = work.vdf_rounds();
        let vdf_output = match tokio::task::spawn_blocking(move || run_vdf(&seed, rounds)).await {
            Ok(output) => output,
            Err(error) => {
                eprintln!("VDF worker failed: {error:#}");
                continue;
            }
        };

        let (mined, outbox) = {
            let mut node = node.lock().await;
            let mined = node.complete_prepared_block(work, vdf_output);
            let outbox = node.drain_outbox();
            (mined, outbox)
        };

        match mined {
            Ok(block) => {
                println!("auto-mined block {} ({})", block.height, block.hash);
            }
            Err(error) => println!("auto-mining skipped after VDF: {error:#}"),
        }

        if let Err(error) = gossip.broadcast(outbox).await {
            eprintln!("p2p broadcast failed after automatic block: {error:#}");
        }

        tokio::task::yield_now().await;
    }
}

async fn run_peer_sync(node: SharedNode, gossip: p2p::GossipNetwork) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let envelopes = {
            let mut node = node.lock().await;
            if !node.chain_started() {
                continue;
            }
            let mut envelopes = vec![node.peer_status()];
            envelopes.extend(node.drain_outbox());
            envelopes.extend(node.mempool_gossip());
            envelopes
        };
        let mut envelopes = envelopes;
        envelopes.push(gossip.peer_exchange().await);
        if let Err(error) = gossip.broadcast(envelopes).await {
            eprintln!("p2p sync gossip failed: {error:#}");
        }
    }
}

async fn run_chain_persistence(node: SharedNode, store: SqliteChainStore) {
    run_chain_persistence_with_interval(node, store, Duration::from_secs(2)).await;
}

async fn run_chain_persistence_with_interval(
    node: SharedNode,
    store: SqliteChainStore,
    interval: Duration,
) {
    let mut last_saved_tip: Option<String> = None;
    loop {
        tokio::time::sleep(interval).await;
        let snapshot = { node.lock().await.chain_snapshot() };
        if !snapshot_chain_started(&snapshot) {
            continue;
        }
        let Some(tip_hash) = snapshot.blocks.last().map(|block| block.hash.clone()) else {
            continue;
        };
        if last_saved_tip.as_deref() == Some(tip_hash.as_str()) {
            continue;
        }

        match persist_chain_snapshot(&store, snapshot).await {
            Ok(()) => last_saved_tip = Some(tip_hash),
            Err(error) => eprintln!("chain persistence failed: {error:#}"),
        }
    }
}

async fn persist_chain_snapshot(store: &SqliteChainStore, snapshot: ChainSnapshot) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save(&snapshot))
        .await
        .context("chain persistence worker failed")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use luun::{
        adapters::{chain_store::SqliteChainStore, config_store::UiConfig},
        app::{DEFAULT_BURN_PER_BLOCK, NodeCore},
        domain::{GenesisBurn, Ledger, MINE_REWARD, Transaction, Wallet},
    };
    use rusqlite::Connection;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use super::{
        ChainMode, CliOptions, help_text, initial_burn_fee, initial_burn_per_block,
        initialize_ledger, persist_chain_snapshot, run_chain_persistence_with_interval,
        setup_ledger, snapshot_chain_started,
    };

    fn parse(args: &[&str]) -> anyhow::Result<Option<CliOptions>> {
        CliOptions::parse_from(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn help_mentions_dev_seed_verify_bypass_env() {
        assert!(help_text().contains("LUUN_DEV_SKIP_SEED_VERIFY=1"));
        assert!(help_text().contains("skip seed verification"));
    }

    fn ledger_with_one_spendable_luun(wallet: &Wallet) -> Ledger {
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 2);
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap()
    }

    fn ledger_with_one_mined_block(wallet: &Wallet) -> Ledger {
        let mut ledger = ledger_with_one_spendable_luun(wallet);
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        ledger
    }

    #[test]
    fn no_args_starts_setup_mode() {
        let opts = parse(&[]).unwrap().unwrap();
        assert_eq!(opts.chain_mode, ChainMode::Setup);
        assert!(opts.join_peers.is_empty());
    }

    #[test]
    fn removed_wallet_seed_is_rejected() {
        let error = parse(&["--wallet-seed", "alice"]).unwrap_err();
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
            let error = parse(&[flag, "value"]).unwrap_err();
            assert!(
                error.to_string().contains("unknown argument"),
                "{flag} should not be accepted"
            );
        }
    }

    #[test]
    fn genesis_flag_is_removed() {
        let error = parse(&["--genesis"]).unwrap_err();
        assert!(error.to_string().contains("unknown argument --genesis"));
    }

    #[test]
    fn join_mode_does_not_start_new_chain() {
        let opts = parse(&["--join", "127.0.0.1:9444"]).unwrap().unwrap();
        assert_eq!(opts.chain_mode, ChainMode::Join);
        assert_eq!(opts.join_peers, vec!["127.0.0.1:9444"]);
    }

    #[test]
    fn setup_mode_starts_with_configured_burn_rate() {
        let configured = UiConfig {
            burn_per_block: 50,
            burn_fee: 3,
            ..UiConfig::default()
        };
        assert_eq!(initial_burn_per_block(&configured), 50);
        assert_eq!(initial_burn_fee(&configured), 3);
        assert_eq!(
            initial_burn_per_block(&UiConfig::default()),
            DEFAULT_BURN_PER_BLOCK
        );
    }

    #[test]
    fn node_can_mine_genesis_from_setup_ledger() {
        let wallet = Wallet::from_seed("genesis-mine-wallet");
        let ledger = setup_ledger();
        let mut node = NodeCore::from_ledger(wallet.clone(), ledger, DEFAULT_BURN_PER_BLOCK);

        assert!(!node.chain_started());
        let genesis = node.mine_genesis().unwrap();

        assert_eq!(genesis.height, 0);
        assert_eq!(genesis.miner, "genesis");
        assert_eq!(genesis.reward, 0);
        assert_eq!(genesis.transactions.len(), 1);
        assert!(matches!(genesis.transactions[0], Transaction::Mine { .. }));
        assert_eq!(genesis.transactions[0].amount(), MINE_REWARD);
        assert_eq!(node.ledger().balance_of(wallet.address()), MINE_REWARD);
        assert!(node.chain_started());
        assert_eq!(node.status().mining.burn_per_block, MINE_REWARD);
        assert_eq!(node.status().mining.automatic_burn_fee, 0);
    }

    #[test]
    fn setup_snapshot_is_not_a_started_chain() {
        let setup = setup_ledger();
        assert!(!setup.is_started());
        assert!(!snapshot_chain_started(&setup.snapshot()));
    }

    #[test]
    fn http_management_port_can_be_configured() {
        let opts = parse(&["--http", "127.0.0.1:18443"]).unwrap().unwrap();
        assert_eq!(opts.http_addr.to_string(), "127.0.0.1:18443");
    }

    #[test]
    fn http_management_port_defaults_to_luun_port() {
        let opts = parse(&[]).unwrap().unwrap();
        assert_eq!(opts.http_addr.to_string(), "127.0.0.1:18661");
    }

    #[test]
    fn wallet_defaults_under_data_dir() {
        let opts = parse(&["--data-dir", "tmp-node"]).unwrap().unwrap();
        assert_eq!(
            opts.wallet_path(),
            std::path::PathBuf::from("tmp-node/wallet.json")
        );
    }

    #[test]
    fn chain_db_defaults_under_data_dir() {
        let opts = parse(&["--data-dir", "tmp-node"]).unwrap().unwrap();
        assert_eq!(
            opts.chain_db_path(),
            std::path::PathBuf::from("tmp-node/chain.sqlite3")
        );
    }

    #[test]
    fn config_defaults_under_data_dir() {
        let opts = parse(&["--data-dir", "tmp-node"]).unwrap().unwrap();
        assert_eq!(
            opts.config_path(),
            std::path::PathBuf::from("tmp-node/config.json")
        );
    }

    #[test]
    fn wallet_path_can_be_explicit() {
        let opts = parse(&["--wallet", "alice-wallet.json"]).unwrap().unwrap();
        assert_eq!(
            opts.wallet_path(),
            std::path::PathBuf::from("alice-wallet.json")
        );
    }

    #[test]
    fn chain_db_path_can_be_explicit() {
        let opts = parse(&["--chain-db", "alice-chain.sqlite3"])
            .unwrap()
            .unwrap();
        assert_eq!(
            opts.chain_db_path(),
            std::path::PathBuf::from("alice-chain.sqlite3")
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

        let resumed = initialize_ledger(&opts, &store).await.unwrap();

        assert_eq!(resumed.status().height, 1);
        assert_eq!(resumed.status().tip_hash, persisted.status().tip_hash);
        assert_eq!(resumed.genesis_hash(), persisted.genesis_hash());
        assert_eq!(resumed.balance_of(fresh_wallet.address()), 0);
    }

    #[tokio::test]
    async fn persisted_chain_satisfies_join_mode_without_contacting_peer() {
        let dir = tempdir().unwrap();
        let chain_path = dir.path().join("chain.sqlite3");
        let store = SqliteChainStore::open(&chain_path).unwrap();
        let alice = Wallet::from_seed("offline-join-alice");
        let persisted = ledger_with_one_mined_block(&alice);
        store.save(&persisted.snapshot()).unwrap();
        let opts = parse(&[
            "--join",
            "127.0.0.1:1",
            "--chain-db",
            chain_path.to_str().unwrap(),
        ])
        .unwrap()
        .unwrap();

        let resumed = initialize_ledger(&opts, &store).await.unwrap();

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
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_json, updated_at_ms)
VALUES (1, 4, 'bad-tip', '{"not":"a chain snapshot"}', 0)
"#,
                [],
            )
            .unwrap();
        let opts = parse(&["--chain-db", chain_path.to_str().unwrap()])
            .unwrap()
            .unwrap();

        let error = initialize_ledger(&opts, &store).await.unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to parse chain snapshot from database"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn persistence_loop_saves_new_tip_after_node_changes() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("background-persistence");
        let ledger = ledger_with_one_spendable_luun(&wallet);
        let node = Arc::new(Mutex::new(NodeCore::from_ledger(
            wallet.clone(),
            ledger,
            DEFAULT_BURN_PER_BLOCK,
        )));
        let initial_snapshot = { node.lock().await.chain_snapshot() };
        persist_chain_snapshot(&store, initial_snapshot)
            .await
            .unwrap();

        let persistence_task = tokio::spawn(run_chain_persistence_with_interval(
            Arc::clone(&node),
            store.clone(),
            Duration::from_millis(10),
        ));
        {
            let mut node = node.lock().await;
            node.burn(1).unwrap();
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
}
