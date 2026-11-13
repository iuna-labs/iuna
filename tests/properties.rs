#![cfg(feature = "e2e")]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use iuna::{
    adapters::{consensus_network::ConsensusNetwork, consensus_store::ConsensusChainStore},
    app::{
        consensus_node::Node,
        wallet_commands::{TicketParameters, Wallet},
    },
    consensus::{
        GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS, Hash, INITIAL_LEDGER_SUBSIDY,
        minimum_ticket_budget,
    },
};
use tokio::{io::AsyncReadExt, sync::Mutex};

const GENESIS_SECRET: [u8; 32] = [
    0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
    0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60,
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires permission to bind local TCP sockets"]
async fn three_nodes_regossip_persist_and_resume_a_heartbeat() {
    let directory = tempfile::tempdir().unwrap();
    let source_store = ConsensusChainStore::open(directory.path().join("source.sqlite3")).unwrap();
    let relay_store = ConsensusChainStore::open(directory.path().join("relay.sqlite3")).unwrap();
    let target_store = ConsensusChainStore::open(directory.path().join("target.sqlite3")).unwrap();
    let source = Arc::new(Mutex::new(Node::open(source_store.clone()).unwrap()));
    let target = Arc::new(Mutex::new(Node::open(target_store.clone()).unwrap()));
    let relay = Arc::new(Mutex::new(Node::open(relay_store).unwrap()));
    let target_network = ConsensusNetwork::start(
        Arc::clone(&target),
        "127.0.0.1:0".parse().unwrap(),
        Vec::new(),
    )
    .await
    .unwrap();
    let relay_network = ConsensusNetwork::start(
        Arc::clone(&relay),
        "127.0.0.1:0".parse().unwrap(),
        vec![target_network.local_addr().into()],
    )
    .await
    .unwrap();
    let source_network = ConsensusNetwork::start(
        Arc::clone(&source),
        "127.0.0.1:0".parse().unwrap(),
        vec![relay_network.local_addr().into()],
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let wallet = Wallet::from_seed(GENESIS_SECRET);
    let ticket = {
        let mut source = source.lock().await;
        let receipt = wallet
            .submit_ticket(
                &mut source,
                TicketParameters {
                    budget: minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap(),
                    duration: 1,
                    fee: 1,
                    mining_public_key: wallet.public_key(),
                    lottery_seed: [60; 32],
                },
            )
            .unwrap();
        source.pending_ticket(&receipt.id).unwrap().clone()
    };
    source_network.broadcast_tickets(vec![ticket]);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if target.lock().await.pending_ticket_count() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let timestamp = GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS;
    let block = source.lock().await.mine_next(timestamp, [61; 32]).unwrap();
    source_network.broadcast_block(&block);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if target.lock().await.status().tip == block.hash() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    source_network.shutdown().await;
    relay_network.shutdown().await;
    target_network.shutdown().await;
    drop(target);

    let resumed = Node::open(target_store).unwrap();
    assert_eq!(resumed.status().tip, block.hash());
    assert_eq!(resumed.status().height, 1);
    assert_eq!(resumed.status().tracked_tickets, 1);
}

type SharedNode = Arc<Mutex<Node<ConsensusChainStore>>>;

const LOCAL_ADDRESS: &str = "127.0.0.1:0";
const SYNC_TIMEOUT: Duration = Duration::from_secs(15);

fn open_node(store: &ConsensusChainStore) -> SharedNode {
    Arc::new(Mutex::new(Node::open(store.clone()).unwrap()))
}

async fn start_network(node: &SharedNode, peers: Vec<SocketAddr>) -> ConsensusNetwork {
    let peers = peers.into_iter().map(Into::into).collect();
    ConsensusNetwork::start(Arc::clone(node), LOCAL_ADDRESS.parse().unwrap(), peers)
        .await
        .unwrap()
}

async fn mine_to(node: &SharedNode, height: u64, miner: [u8; 32]) -> Hash {
    let mut node = node.lock().await;
    while node.status().height < height {
        // Peers check this against the real clock, and genesis may be recent.
        let timestamp = node.chain().tip().tip_timestamp + 1;
        node.mine_next(timestamp, miner).unwrap();
    }
    node.status().tip
}

async fn wait_for_tip(node: &SharedNode, tip: Hash) {
    tokio::time::timeout(SYNC_TIMEOUT, async {
        loop {
            if node.lock().await.status().tip == tip {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

/// Two islands build competing branches that differ by more than one sync batch. When the link
/// heals, the lighter island reorganizes, and its relay-only member follows through gossip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires permission to bind local TCP sockets"]
async fn healed_partition_converges_on_the_heavier_branch() {
    let directory = tempfile::tempdir().unwrap();
    let store = |name: &str| ConsensusChainStore::open(directory.path().join(name)).unwrap();
    let heavy = open_node(&store("heavy.sqlite3"));
    let light = open_node(&store("light.sqlite3"));
    let follower = open_node(&store("follower.sqlite3"));
    let heavy_tip = mine_to(&heavy, 12, [62; 32]).await;
    let light_tip = mine_to(&light, 10, [63; 32]).await;

    let heavy_network = start_network(&heavy, Vec::new()).await;
    let light_network = start_network(&light, Vec::new()).await;
    let follower_network = start_network(&follower, vec![light_network.local_addr()]).await;
    wait_for_tip(&follower, light_tip).await;
    assert_eq!(heavy.lock().await.status().tip, heavy_tip);

    light_network
        .peer_registry()
        .add_target(heavy_network.local_addr())
        .unwrap();
    wait_for_tip(&light, heavy_tip).await;
    wait_for_tip(&follower, heavy_tip).await;

    let next_tip = mine_to(&light, 13, [64; 32]).await;
    let block = light.lock().await.chain().block(&next_tip).unwrap().clone();
    light_network.broadcast_block(&block);
    wait_for_tip(&heavy, next_tip).await;
    wait_for_tip(&follower, next_tip).await;

    follower_network.shutdown().await;
    light_network.shutdown().await;
    heavy_network.shutdown().await;
}

/// A restarted node resumes from its store, catches up on what it missed over the network and then
/// keeps following new blocks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires permission to bind local TCP sockets"]
async fn restarted_node_catches_up_and_keeps_following() {
    let directory = tempfile::tempdir().unwrap();
    let source_store = ConsensusChainStore::open(directory.path().join("source.sqlite3")).unwrap();
    let store = ConsensusChainStore::open(directory.path().join("restarted.sqlite3")).unwrap();
    let source = open_node(&source_store);
    let restarted = open_node(&store);
    let source_network = start_network(&source, Vec::new()).await;
    let peers = vec![source_network.local_addr()];

    let network = start_network(&restarted, peers.clone()).await;
    let first_tip = mine_to(&source, 3, [65; 32]).await;
    let block = source
        .lock()
        .await
        .chain()
        .block(&first_tip)
        .unwrap()
        .clone();
    source_network.broadcast_block(&block);
    wait_for_tip(&restarted, first_tip).await;
    network.shutdown().await;
    drop(restarted);

    let missed_tip = mine_to(&source, 13, [65; 32]).await;
    let restarted = open_node(&store);
    assert_eq!(restarted.lock().await.status().tip, first_tip);
    let network = start_network(&restarted, peers).await;
    wait_for_tip(&restarted, missed_tip).await;

    let next_tip = mine_to(&source, 14, [65; 32]).await;
    let block = source
        .lock()
        .await
        .chain()
        .block(&next_tip)
        .unwrap()
        .clone();
    source_network.broadcast_block(&block);
    wait_for_tip(&restarted, next_tip).await;

    network.shutdown().await;
    source_network.shutdown().await;
}

/// Published bootstrap nodes are host names, so a node must sync from one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires permission to bind local TCP sockets"]
async fn a_node_syncs_from_a_peer_given_by_host_name() {
    let directory = tempfile::tempdir().unwrap();
    let store = |name: &str| ConsensusChainStore::open(directory.path().join(name)).unwrap();
    let source = open_node(&store("source.sqlite3"));
    let target = open_node(&store("target.sqlite3"));
    let tip = mine_to(&source, 3, [67; 32]).await;
    let source_network = start_network(&source, Vec::new()).await;

    let peer = format!("localhost:{}", source_network.local_addr().port())
        .parse()
        .unwrap();
    let target_network = ConsensusNetwork::start(
        Arc::clone(&target),
        LOCAL_ADDRESS.parse().unwrap(),
        vec![peer],
    )
    .await
    .unwrap();
    wait_for_tip(&target, tip).await;

    target_network.shutdown().await;
    source_network.shutdown().await;
}

/// A connection that never says hello must give its inbound slot back, so the same host can still
/// connect once its silent connections are gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires permission to bind local TCP sockets"]
async fn silent_inbound_connections_give_their_slots_back() {
    let directory = tempfile::tempdir().unwrap();
    let store = |name: &str| ConsensusChainStore::open(directory.path().join(name)).unwrap();
    let source = open_node(&store("source.sqlite3"));
    let target = open_node(&store("target.sqlite3"));
    let tip = mine_to(&source, 1, [68; 32]).await;
    let network = start_network(&source, Vec::new()).await;

    // More than one host may hold, so a leaked permit would lock this address out below.
    let mut silent = Vec::new();
    for _ in 0..9 {
        silent.push(
            tokio::net::TcpStream::connect(network.local_addr())
                .await
                .unwrap(),
        );
    }
    for connection in &mut silent {
        let mut received = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(25),
            connection.read_to_end(&mut received),
        )
        .await
        .expect("the node must close a silent connection")
        .ok();
    }

    let follower = start_network(&target, vec![network.local_addr()]).await;
    wait_for_tip(&target, tip).await;

    follower.shutdown().await;
    network.shutdown().await;
}
