use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use iuna::{
    adapters::{chain_store::SqliteChainStore, p2p::GossipNetwork, stratum::StratumServer},
    app::{NodeCore, PeerBook, SharedNode, now_ms},
    domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet, run_vdf},
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::Mutex,
    time::{sleep, timeout},
};

const SOAK_BLOCKS: u64 = 12;
const BURN_COLLECTION_MS: u64 = 31_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running release-mode soak; run with cargo test --release --test properties -- --ignored"]
async fn release_soak_auto_finalization_p2p_stratum_and_restarts() -> Result<()> {
    let wallets = (0..3)
        .map(|index| Wallet::from_seed(&format!("release-soak-wallet-{index}")))
        .collect::<Vec<_>>();
    let genesis = funded_ledger(&wallets);
    let p2p_addrs = reserve_loopback_addrs(wallets.len())?;
    let stratum_addr = reserve_loopback_addrs(1)?.remove(0);
    let store_dirs = (0..wallets.len())
        .map(|_| tempdir())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let stores = store_dirs
        .iter()
        .map(|dir| SqliteChainStore::open(dir.path().join("chain.sqlite3")))
        .collect::<Result<Vec<_>>>()?;
    let mut nodes = Vec::new();

    for (index, wallet) in wallets.iter().cloned().enumerate() {
        let burn_per_block = if index == 0 { 2 } else { 0 };
        let mut core = NodeCore::from_ledger_with_burn_fee_and_enabled(
            wallet,
            genesis.clone(),
            true,
            burn_per_block,
            1,
        );
        core.set_recovery_vdf_top_rank_percent(0);
        let node = Arc::new(Mutex::new(core));
        let peer_addresses = p2p_addrs
            .iter()
            .enumerate()
            .filter(|(peer_index, _)| *peer_index != index)
            .map(|(_, addr)| addr.to_string())
            .collect::<Vec<_>>();
        let peers = Arc::new(Mutex::new(PeerBook::from_addresses(peer_addresses)));
        let network =
            GossipNetwork::start(node.clone(), peers, p2p_addrs[index], None, true).await?;
        nodes.push(SoakNode {
            wallet: wallets[index].clone(),
            burn_per_block,
            node,
            network,
            store: stores[index].clone(),
        });
    }

    let _stratum = StratumServer::start(
        nodes[0].node.clone(),
        nodes[0].network.clone(),
        stratum_addr,
    )
    .await?;
    let stratum_worker = nodes[1]
        .node
        .lock()
        .await
        .wallet_receive_address()
        .context("release-soak wallet must have a checksummed receive address")?;
    assert_stratum_serves_work(stratum_addr, &stratum_worker).await?;
    sleep(Duration::from_secs(2)).await;

    for target_height in 1..=SOAK_BLOCKS {
        finalize_one_block(&nodes, target_height).await?;
        wait_for_convergence(&nodes, target_height, Duration::from_secs(8)).await?;

        if target_height % 3 == 0 {
            restart_node_core(&nodes[1]).await?;
            wait_for_convergence(&nodes, target_height, Duration::from_secs(8)).await?;
        }
        if target_height % 4 == 0 {
            restart_node_core(&nodes[2]).await?;
            wait_for_convergence(&nodes, target_height, Duration::from_secs(8)).await?;
        }
    }

    let final_tip = nodes[0].node.lock().await.chain_tip_hash();
    for node in &nodes {
        assert_eq!(node.node.lock().await.chain_tip_hash(), final_tip);
    }
    assert!(configured_automatic_burn_was_included(&nodes[0]).await);
    Ok(())
}

struct SoakNode {
    wallet: Wallet,
    burn_per_block: u64,
    node: SharedNode,
    network: GossipNetwork,
    store: SqliteChainStore,
}

fn funded_ledger(wallets: &[Wallet]) -> Ledger {
    let allocations = wallets
        .iter()
        .map(|wallet| (wallet.address().to_string(), 100 * MICRO_IUNA))
        .collect::<BTreeMap<_, _>>();
    let genesis_burns = wallets
        .iter()
        .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
        .collect::<Vec<_>>();
    Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap()
}

fn reserve_loopback_addrs(count: usize) -> Result<Vec<SocketAddr>> {
    let mut listeners = Vec::new();
    let mut addrs = Vec::new();
    for _ in 0..count {
        let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        addrs.push(listener.local_addr()?);
        listeners.push(listener);
    }
    drop(listeners);
    Ok(addrs)
}

async fn assert_stratum_serves_work(addr: SocketAddr, worker: &str) -> Result<()> {
    let stream = timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    write
        .write_all(
            json_line(json!({"id": 1, "method": "mining.subscribe", "params": []}))?.as_bytes(),
        )
        .await?;
    write
        .write_all(
            json_line(json!({"id": 2, "method": "mining.authorize", "params": [worker, "x"]}))?
                .as_bytes(),
        )
        .await?;

    let mut authorized = false;
    let mut notified = false;
    for _ in 0..4 {
        let line = timeout(Duration::from_secs(5), lines.next_line())
            .await??
            .context("stratum server closed before sending work")?;
        let value: Value = serde_json::from_str(&line)?;
        authorized |=
            value.get("id") == Some(&json!(2)) && value.get("result") == Some(&json!(true));
        notified |= value.get("method") == Some(&json!("mining.notify"));
        if authorized && notified {
            return Ok(());
        }
    }
    bail!("stratum did not authorize and send mining.notify")
}

fn json_line(value: Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string(&value)?))
}

async fn finalize_one_block(nodes: &[SoakNode], target_height: u64) -> Result<()> {
    let start = now_ms().saturating_sub(BURN_COLLECTION_MS + 1);
    prepare_and_broadcast(nodes, start).await?;
    sleep(Duration::from_millis(250)).await;
    let timestamp_ms = now_ms();
    prepare_and_broadcast(nodes, timestamp_ms).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        for node in nodes {
            if let Some(block) = complete_if_ready(node, now_ms()).await? {
                node.network
                    .broadcast(node.node.lock().await.drain_outbox())
                    .await?;
                assert_eq!(block.height, target_height);
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "no node finalized block {target_height}:\n{}",
                soak_diagnostics(nodes, target_height).await
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn soak_diagnostics(nodes: &[SoakNode], target_height: u64) -> String {
    let mut lines = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let core = node.node.lock().await;
        let status = core.status();
        let rank = core
            .burn_leader_ranks_for_block(target_height)
            .ok()
            .and_then(|ranks| {
                ranks
                    .into_iter()
                    .find(|rank| rank.owner == core.wallet_address())
                    .map(|rank| rank.rank)
            });
        let pending = core.pending_transactions();
        let pending_burns = pending.iter().filter(|tx| tx.is_burn()).count();
        let metrics = node.network.metrics();
        lines.push(format!(
            "node {index}: height={}, tip={}, wallet_rank={rank:?}, leader={:?}, \
             last_finalization={:?}, pending={} (burns={pending_burns}), \
             burn_bundles_received={}, control_received={}, rejected_blocks={}, \
             session_failures={}, last_session_failure={:?}, last_chain_error={:?}, \
             sync_progress={:?}",
            status.chain.height,
            status.chain.tip_hash,
            status.mining.current_leader,
            status.mining.last_auto_finalization_status,
            pending.len(),
            metrics.burn_bundles_received,
            metrics.control_envelopes_received,
            metrics.rejected_blocks,
            metrics.session_failures,
            metrics.last_session_failure,
            metrics.last_chain_payload_error,
            node.network.sync_progress(),
        ));
    }
    lines.join("\n")
}

async fn prepare_and_broadcast(nodes: &[SoakNode], timestamp_ms: u64) -> Result<()> {
    for node in nodes {
        {
            let mut core = node.node.lock().await;
            let _ = core.prepare_automatic_finalization(timestamp_ms);
        }
        node.network
            .broadcast(node.node.lock().await.drain_outbox())
            .await?;
    }
    Ok(())
}

async fn complete_if_ready(
    node: &SoakNode,
    timestamp_ms: u64,
) -> Result<Option<iuna::domain::Block>> {
    let work = {
        let mut core = node.node.lock().await;
        core.prepare_automatic_finalization(timestamp_ms).work
    };
    let Some(work) = work else {
        return Ok(None);
    };
    let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
    let block =
        node.node
            .lock()
            .await
            .complete_prepared_block_at(work, vdf_output, timestamp_ms)?;
    Ok(Some(block))
}

async fn wait_for_convergence(nodes: &[SoakNode], height: u64, duration: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let mut tips = Vec::new();
        for node in nodes {
            let core = node.node.lock().await;
            tips.push((core.chain_height(), core.chain_tip_hash()));
        }
        if tips
            .iter()
            .all(|(node_height, tip)| *node_height >= height && tip == &tips[0].1)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("nodes did not converge at height {height}: {tips:?}");
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn configured_automatic_burn_was_included(node: &SoakNode) -> bool {
    node.node
        .lock()
        .await
        .chain()
        .iter()
        .flat_map(|block| &block.transactions)
        .any(|tx| tx.is_burn() && tx.sender() == node.wallet.address() && tx.amount() == 2)
}

async fn restart_node_core(node: &SoakNode) -> Result<()> {
    let snapshot = node.node.lock().await.chain_snapshot();
    node.store.save(&snapshot)?;
    let restored = Ledger::from_persisted_snapshot(
        node.store
            .load()?
            .context("persisted snapshot should exist after save")?,
    )?;
    let mut restored_node = NodeCore::from_ledger_with_burn_fee_and_enabled(
        node.wallet.clone(),
        restored,
        true,
        node.burn_per_block,
        1,
    );
    restored_node.set_recovery_vdf_top_rank_percent(0);
    *node.node.lock().await = restored_node;
    Ok(())
}
