use std::{
    collections::BTreeMap, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc, time::Duration,
};

use anyhow::{Context, Result, bail};
use mivora::{
    adapters::{chain_store::SqliteChainStore, http, p2p, wallet_store},
    app::{
        DEFAULT_BURN_PER_BLOCK, DEFAULT_VDF_ROUNDS, NodeCore, PeerBook, SharedNode, SharedPeerBook,
        now_ms,
    },
    domain::{Amount, ChainSnapshot, GenesisBurn, Ledger, run_vdf},
};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    let Some(opts) = CliOptions::parse()? else {
        return Ok(());
    };
    let wallet_path = opts.wallet_path();
    let wallet = wallet_store::load_or_create(&wallet_path)?;
    let chain_store = SqliteChainStore::open(opts.chain_db_path())?;
    let ledger = initialize_ledger(&opts, wallet.address(), &chain_store).await?;

    let node: SharedNode = Arc::new(Mutex::new(NodeCore::from_ledger(
        wallet,
        ledger,
        DEFAULT_BURN_PER_BLOCK,
    )));
    let peers: SharedPeerBook = Arc::new(Mutex::new(PeerBook::from_addresses(opts.peers)));
    let initial_snapshot = { node.lock().await.chain_snapshot() };
    persist_chain_snapshot(&chain_store, initial_snapshot).await?;

    println!("mivora wallet: {}", node.lock().await.wallet_address());
    println!("wallet file: {}", wallet_path.display());
    println!("chain database: {}", chain_store.path().display());
    println!("management UI: http://{}", opts.http_addr);
    println!("p2p listener: {}", opts.p2p_addr);
    println!(
        "automatic mining: VDF-driven, burning {} coins per block",
        DEFAULT_BURN_PER_BLOCK
    );

    let persistence_node = Arc::clone(&node);
    let persistence_store = chain_store.clone();
    tokio::spawn(async move {
        run_chain_persistence(persistence_node, persistence_store).await;
    });

    let gossip =
        p2p::GossipNetwork::start(Arc::clone(&node), Arc::clone(&peers), opts.p2p_addr).await?;

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

    http::serve(node, peers, gossip, opts.http_addr).await
}

async fn initialize_ledger(
    opts: &CliOptions,
    wallet_address: &str,
    chain_store: &SqliteChainStore,
) -> Result<Ledger> {
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
    } else if opts.start_new_chain {
        start_new_chain_ledger(wallet_address, opts.genesis_amount, opts.vdf_rounds)
    } else {
        join_chain_ledger(&opts.join_peers, opts.p2p_addr).await
    }
}

#[derive(Debug)]
struct CliOptions {
    wallet_path: Option<PathBuf>,
    chain_db_path: Option<PathBuf>,
    http_addr: SocketAddr,
    p2p_addr: SocketAddr,
    peers: Vec<String>,
    join_peers: Vec<String>,
    start_new_chain: bool,
    genesis_amount: Amount,
    vdf_rounds: u32,
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
            http_addr: SocketAddr::from_str("127.0.0.1:8443")?,
            p2p_addr: SocketAddr::from_str("127.0.0.1:9444")?,
            peers: Vec::new(),
            join_peers: Vec::new(),
            start_new_chain: false,
            genesis_amount: 1,
            vdf_rounds: DEFAULT_VDF_ROUNDS,
            data_dir: PathBuf::from(".mivora"),
        };

        let raw_args = args.into_iter().collect::<Vec<_>>();
        if raw_args.is_empty() {
            print_help();
            return Ok(None);
        }

        let mut args = raw_args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--start" => opts.start_new_chain = true,
                "--wallet" => {
                    opts.wallet_path = Some(PathBuf::from(next_value(&mut args, "--wallet")?))
                }
                "--chain-db" => {
                    opts.chain_db_path = Some(PathBuf::from(next_value(&mut args, "--chain-db")?))
                }
                "--wallet-seed" => {
                    bail!(
                        "--wallet-seed was removed; wallets are stored in --wallet <path> or .mivora/wallet.json"
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
                    opts.peers.push(peer.clone());
                    opts.join_peers.push(peer);
                }
                "--genesis-amount" => {
                    opts.genesis_amount = next_value(&mut args, "--genesis-amount")?
                        .parse()
                        .context("invalid --genesis-amount")?;
                    if opts.genesis_amount < 1 {
                        bail!("--genesis-amount must be at least 1 for the genesis ticket");
                    }
                }
                "--vdf-rounds" => {
                    opts.vdf_rounds = next_value(&mut args, "--vdf-rounds")?
                        .parse()
                        .context("invalid --vdf-rounds")?;
                }
                "--data-dir" => opts.data_dir = PathBuf::from(next_value(&mut args, "--data-dir")?),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}; pass --help for usage"),
            }
        }

        if opts.start_new_chain && !opts.join_peers.is_empty() {
            bail!("choose either --start or --join, not both");
        }
        if !opts.start_new_chain && opts.join_peers.is_empty() {
            bail!("pass --start to create a new chain, or --join <addr:port> to join one");
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
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value after {flag}"))
}

fn print_help() {
    println!(
        "mivora\n\n\
         Usage:\n\
           mivora --start [options]\n\
           mivora --join <addr:port> [options]\n\n\
         Options:\n\
           --start                       Create a new chain with a genesis burn\n\
           --wallet <path>               Wallet file (default <data-dir>/wallet.json)\n\
           --chain-db <path>             Chain SQLite database (default <data-dir>/chain.sqlite3)\n\
           --http <addr:port>            HTTP management UI address (default 127.0.0.1:8443)\n\
           --p2p <addr:port>             P2P TCP listener address (default 127.0.0.1:9444)\n\
           --join <addr:port>            Fetch chain snapshot from this peer before mining\n\
           --genesis-amount <amount>     Genesis allocation before the 1-coin bootstrap ticket (default 1)\n\
           --vdf-rounds <rounds>         Initial VDF delay rounds; protocol retargets toward 60s blocks\n\
           --data-dir <path>             Local wallet directory\n"
    );
}

fn snapshot_height(snapshot: &ChainSnapshot) -> u64 {
    snapshot
        .blocks
        .last()
        .map(|block| block.height)
        .unwrap_or(0)
}

fn start_new_chain_ledger(
    wallet_address: &str,
    genesis_amount: Amount,
    vdf_rounds: u32,
) -> Result<Ledger> {
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet_address.to_string(), genesis_amount);
    Ledger::new_with_genesis_burns(
        genesis,
        vec![GenesisBurn::new(wallet_address, 1)],
        vdf_rounds,
    )
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

    use mivora::{
        adapters::chain_store::SqliteChainStore,
        app::{DEFAULT_BURN_PER_BLOCK, NodeCore},
        domain::{GenesisBurn, Ledger, Wallet},
    };
    use rusqlite::Connection;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use super::{
        CliOptions, initialize_ledger, persist_chain_snapshot, run_chain_persistence_with_interval,
    };

    fn parse(args: &[&str]) -> anyhow::Result<Option<CliOptions>> {
        CliOptions::parse_from(args.iter().map(|arg| arg.to_string()))
    }

    fn ledger_with_one_spendable_coin(wallet: &Wallet) -> Ledger {
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 2);
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap()
    }

    fn ledger_with_one_mined_block(wallet: &Wallet) -> Ledger {
        let mut ledger = ledger_with_one_spendable_coin(wallet);
        ledger
            .submit_transaction(wallet.burn(1, ledger.next_nonce(wallet.address())))
            .unwrap();
        let block = ledger.mine_next_block(wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        ledger
    }

    #[test]
    fn no_args_prints_help_without_starting() {
        assert!(parse(&[]).unwrap().is_none());
    }

    #[test]
    fn removed_wallet_seed_is_rejected() {
        let error = parse(&["--wallet-seed", "alice", "--start"]).unwrap_err();
        assert!(error.to_string().contains("--wallet-seed was removed"));
    }

    #[test]
    fn runtime_configuration_flags_are_rejected() {
        for flag in ["--name", "--burn-per-block", "--peer"] {
            let error = parse(&["--start", flag, "value"]).unwrap_err();
            assert!(
                error.to_string().contains("unknown argument"),
                "{flag} should not be accepted"
            );
        }
    }

    #[test]
    fn start_mode_is_explicit() {
        let opts = parse(&["--start"]).unwrap().unwrap();
        assert!(opts.start_new_chain);
        assert!(opts.join_peers.is_empty());
    }

    #[test]
    fn join_mode_does_not_start_new_chain() {
        let opts = parse(&["--join", "127.0.0.1:9444"]).unwrap().unwrap();
        assert!(!opts.start_new_chain);
        assert_eq!(opts.join_peers, vec!["127.0.0.1:9444"]);
    }

    #[test]
    fn http_management_port_can_be_configured() {
        let opts = parse(&["--start", "--http", "127.0.0.1:18443"])
            .unwrap()
            .unwrap();
        assert_eq!(opts.http_addr.to_string(), "127.0.0.1:18443");
    }

    #[test]
    fn wallet_defaults_under_data_dir() {
        let opts = parse(&["--start", "--data-dir", "tmp-node"])
            .unwrap()
            .unwrap();
        assert_eq!(
            opts.wallet_path(),
            std::path::PathBuf::from("tmp-node/wallet.json")
        );
    }

    #[test]
    fn chain_db_defaults_under_data_dir() {
        let opts = parse(&["--start", "--data-dir", "tmp-node"])
            .unwrap()
            .unwrap();
        assert_eq!(
            opts.chain_db_path(),
            std::path::PathBuf::from("tmp-node/chain.sqlite3")
        );
    }

    #[test]
    fn wallet_path_can_be_explicit() {
        let opts = parse(&["--start", "--wallet", "alice-wallet.json"])
            .unwrap()
            .unwrap();
        assert_eq!(
            opts.wallet_path(),
            std::path::PathBuf::from("alice-wallet.json")
        );
    }

    #[test]
    fn chain_db_path_can_be_explicit() {
        let opts = parse(&["--start", "--chain-db", "alice-chain.sqlite3"])
            .unwrap()
            .unwrap();
        assert_eq!(
            opts.chain_db_path(),
            std::path::PathBuf::from("alice-chain.sqlite3")
        );
    }

    #[tokio::test]
    async fn startup_resumes_persisted_chain_instead_of_creating_new_genesis() {
        let dir = tempdir().unwrap();
        let chain_path = dir.path().join("chain.sqlite3");
        let store = SqliteChainStore::open(&chain_path).unwrap();
        let persisted_wallet = Wallet::from_seed("persisted-chain-owner");
        let persisted = ledger_with_one_mined_block(&persisted_wallet);
        store.save(&persisted.snapshot()).unwrap();
        let fresh_wallet = Wallet::from_seed("fresh-start-wallet");
        let opts = parse(&[
            "--start",
            "--genesis-amount",
            "1",
            "--chain-db",
            chain_path.to_str().unwrap(),
        ])
        .unwrap()
        .unwrap();

        let resumed = initialize_ledger(&opts, fresh_wallet.address(), &store)
            .await
            .unwrap();

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
        let bob = Wallet::from_seed("offline-join-bob");
        let opts = parse(&[
            "--join",
            "127.0.0.1:1",
            "--chain-db",
            chain_path.to_str().unwrap(),
        ])
        .unwrap()
        .unwrap();

        let resumed = initialize_ledger(&opts, bob.address(), &store)
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
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_json, updated_at_ms)
VALUES (1, 4, 'bad-tip', '{"not":"a chain snapshot"}', 0)
"#,
                [],
            )
            .unwrap();
        let wallet = Wallet::from_seed("bad-db-wallet");
        let opts = parse(&[
            "--start",
            "--genesis-amount",
            "2",
            "--chain-db",
            chain_path.to_str().unwrap(),
        ])
        .unwrap()
        .unwrap();

        let error = initialize_ledger(&opts, wallet.address(), &store)
            .await
            .unwrap_err();

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
        let ledger = ledger_with_one_spendable_coin(&wallet);
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
