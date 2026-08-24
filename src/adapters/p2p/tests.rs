use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
};

use crate::{
    app::{
        GossipEnvelope, NETWORK_ID, NodeCore, PROTOCOL_VERSION, PeerBook, PeerDirection,
        ProtocolHello,
    },
    domain::{Ledger, Wallet, run_vdf},
};
use tokio::io::AsyncWriteExt;

use super::test_support::{allocations, gossip_network, node, queue_plaintext_burn};

#[tokio::test]
async fn block_batch_validation_reports_each_validated_height() {
    let alice = Wallet::from_seed("batch-validation-progress-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let local = node("batch-progress-local", alice.clone(), allocations.clone());
    let mut remote = node("batch-progress-remote", alice.clone(), allocations);
    for timestamp_ms in [1, 2] {
        queue_plaintext_burn(&mut remote, &alice, 1);
        remote.drain_outbox();
        remote.mine_one_at(timestamp_ms).unwrap();
        remote.drain_outbox();
    }
    let blocks = remote.ledger().blocks_from(1, 10);
    let progress = Arc::new(StdMutex::new(Vec::new()));
    let reported = Arc::clone(&progress);

    let adopted = super::validate_blocks_extension(
        local.clone_ledger(),
        blocks,
        crate::app::now_ms(),
        move |height| reported.lock().unwrap().push(height),
    )
    .await
    .unwrap();

    assert_eq!(*progress.lock().unwrap(), vec![1, 2]);
    assert_eq!(adopted.height(), 2);
}

#[tokio::test]
async fn full_outbound_queue_is_metric_not_peer_error() {
    let wallet = Wallet::from_seed("full-outbound-queue");
    let node = Arc::new(tokio::sync::Mutex::new(node(
        "full-outbound-queue",
        wallet.clone(),
        allocations(&[wallet], 1_000),
    )));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
        "127.0.0.1:9444".to_string(),
    ])));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::clone(&peers),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    sender
        .try_send(vec![GossipEnvelope::PeerStatus {
            height: 1,
            tip_hash: "queued".to_string(),
            time_ms: 1_000,
        }])
        .unwrap();
    network
        .inner
        .sessions
        .lock()
        .await
        .insert("127.0.0.1:9444".to_string(), sender);

    network
        .broadcast(vec![GossipEnvelope::PeerStatus {
            height: 2,
            tip_hash: "new".to_string(),
            time_ms: 2_000,
        }])
        .await
        .unwrap();

    assert_eq!(network.metrics().outbound_queue_full, 1);
    let peer = peers.lock().await.list().pop().unwrap();
    assert_eq!(peer.last_error, None);
    assert_eq!(peer.last_error_ms, None);
}

#[tokio::test]
async fn single_block_fork_error_requests_blocks_by_locator() {
    let alice = Wallet::from_seed("single-block-fork-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let mut local_node = node(
        "local-single-block-fork",
        alice.clone(),
        allocations.clone(),
    );
    let mut remote_node = node("remote-single-block-fork", alice.clone(), allocations);

    queue_plaintext_burn(&mut local_node, &alice, 1);
    local_node.drain_outbox();
    local_node.mine_one_at(1).unwrap();
    local_node.drain_outbox();

    queue_plaintext_burn(&mut remote_node, &alice, 1);
    remote_node.drain_outbox();
    remote_node.mine_one_at(2).unwrap();
    remote_node.drain_outbox();
    queue_plaintext_burn(&mut remote_node, &alice, 1);
    remote_node.drain_outbox();
    let remote_block = remote_node.mine_one_at(3).unwrap();
    assert_eq!(remote_block.height, 2);
    assert_ne!(
        remote_block.prev_hash,
        local_node.ledger().tip_hash().to_string()
    );

    let network = gossip_network(
        Arc::new(tokio::sync::Mutex::new(local_node)),
        Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
        "127.0.0.1:9544".parse().unwrap(),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, remote_addr) = listener.accept().await.unwrap();
    let (_server_reader, mut server_writer) = server.into_split();
    let (client_reader, _client_writer) = client.into_split();
    let mut client_reader = super::LimitedLineReader::new(client_reader);
    let mut known_peer = None;

    super::process_envelope(
        &network,
        &mut server_writer,
        remote_addr,
        &mut known_peer,
        GossipEnvelope::Block(remote_block),
    )
    .await
    .unwrap();

    let line = tokio::time::timeout(std::time::Duration::from_secs(1), client_reader.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let GossipEnvelope::BlockLocatorRequest { locator, limit } =
        super::parse_envelope(&line).unwrap()
    else {
        panic!("expected block locator request");
    };
    let fork_blocks = remote_node.blocks_after_locator(&locator, limit);
    assert_eq!(fork_blocks.len(), 2);
    let local_ledger = network.inner.node.lock().await.clone_ledger();
    let adopted =
        super::validate_blocks_extension(local_ledger, fork_blocks, crate::app::now_ms(), |_| {})
            .await
            .unwrap();
    assert_eq!(adopted.tip_hash(), remote_node.ledger().tip_hash());
}

#[tokio::test]
async fn future_block_rejection_does_not_poison_peer_or_later_acceptance() {
    let alice = Wallet::from_seed("future-block-p2p-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let mut producer = node("future-block-producer", alice.clone(), allocations.clone());
    queue_plaintext_burn(&mut producer, &alice, 1);
    producer.drain_outbox();
    let future_timestamp = crate::app::now_ms().saturating_add(10 * 60 * 1_000);
    let prepared = producer
        .ledger()
        .prepare_next_block(alice.address(), future_timestamp)
        .unwrap();
    let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
    let future_block = prepared.finish(&alice, vdf_output);

    let local_node = node("future-block-local", alice.clone(), allocations);
    let original_tip = local_node.ledger().tip_hash().to_string();
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = gossip_network(
        Arc::new(tokio::sync::Mutex::new(local_node)),
        Arc::clone(&peers),
        "127.0.0.1:9544".parse().unwrap(),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, remote_addr) = listener.accept().await.unwrap();
    let (_server_reader, mut server_writer) = server.into_split();
    let (_client_reader, _client_writer) = client.into_split();
    let peer = remote_addr.to_string();
    let mut known_peer = Some(peer.clone());

    super::process_envelope(
        &network,
        &mut server_writer,
        remote_addr,
        &mut known_peer,
        GossipEnvelope::Block(future_block.clone()),
    )
    .await
    .unwrap();

    assert_eq!(
        network.inner.node.lock().await.ledger().tip_hash(),
        original_tip
    );
    assert_eq!(network.metrics().rejected_blocks, 1);
    let recorded_peer = peers
        .lock()
        .await
        .list()
        .into_iter()
        .find(|entry| entry.address == peer)
        .expect("future block sender should be recorded");
    assert_eq!(recorded_peer.misbehavior_score, 0);
    assert!(recorded_peer.banned_until_ms.is_none());
    assert!(
        recorded_peer
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("too far in the future"))
    );

    let verified_block = super::verify_block_vdf(future_block.clone()).await.unwrap();
    network
        .inner
        .node
        .lock()
        .await
        .receive_preverified_block_at(verified_block, future_block.timestamp_ms)
        .unwrap();
    assert_eq!(
        network.inner.node.lock().await.ledger().tip_hash(),
        future_block.hash
    );
}

#[tokio::test]
async fn invalid_block_batch_is_rejected_atomically_without_partial_import() {
    let alice = Wallet::from_seed("invalid-batch-p2p-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let mut remote_node = node("invalid-batch-remote", alice.clone(), allocations.clone());
    queue_plaintext_burn(&mut remote_node, &alice, 1);
    remote_node.drain_outbox();
    let first_block = remote_node.mine_one_at(1).unwrap();
    remote_node.drain_outbox();
    queue_plaintext_burn(&mut remote_node, &alice, 1);
    remote_node.drain_outbox();
    let second_block = remote_node.mine_one_at(2).unwrap();

    let mut invalid_second_block = second_block.clone();
    invalid_second_block.reward = invalid_second_block.reward.saturating_add(1);
    invalid_second_block.hash = invalid_second_block.compute_hash();

    let local_node = node("invalid-batch-local", alice, allocations);
    let original_tip = local_node.ledger().tip_hash().to_string();
    let original_height = local_node.ledger().height();
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = gossip_network(
        Arc::new(tokio::sync::Mutex::new(local_node)),
        Arc::clone(&peers),
        "127.0.0.1:9544".parse().unwrap(),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, remote_addr) = listener.accept().await.unwrap();
    let (_server_reader, mut server_writer) = server.into_split();
    let (_client_reader, _client_writer) = client.into_split();
    let peer = remote_addr.to_string();
    let mut known_peer = Some(peer.clone());

    super::process_envelope(
        &network,
        &mut server_writer,
        remote_addr,
        &mut known_peer,
        GossipEnvelope::Blocks {
            blocks: vec![first_block.clone(), invalid_second_block],
        },
    )
    .await
    .unwrap();

    let node = network.inner.node.lock().await;
    assert_eq!(node.ledger().height(), original_height);
    assert_eq!(node.ledger().tip_hash(), original_tip);
    assert!(!node.ledger().has_block(&first_block.hash));
    drop(node);

    assert_eq!(network.metrics().rejected_block_batches, 1);
    assert!(
        network
            .metrics()
            .last_chain_payload_error
            .as_deref()
            .is_some_and(|error| error.contains("block batch: block reward is invalid"))
    );
    let recorded_peer = peers
        .lock()
        .await
        .list()
        .into_iter()
        .find(|entry| entry.address == peer)
        .expect("invalid batch sender should be recorded");
    assert_eq!(recorded_peer.misbehavior_score, 1);
    assert!(
        recorded_peer
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("block reward is invalid"))
    );
}

#[tokio::test]
async fn chain_bootstrap_request_only_writes_bootstrap_without_mutating_local_state() {
    let alice = Wallet::from_seed("snapshot-request-spam-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let mut local_node = node("snapshot-request-spam", alice.clone(), allocations);
    queue_plaintext_burn(&mut local_node, &alice, 1);
    local_node.drain_outbox();
    local_node.mine_one_at(1).unwrap();
    local_node.drain_outbox();
    let before = local_node.chain_snapshot();

    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = gossip_network(
        Arc::new(tokio::sync::Mutex::new(local_node)),
        Arc::clone(&peers),
        "127.0.0.1:9544".parse().unwrap(),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, remote_addr) = listener.accept().await.unwrap();
    let (_server_reader, mut server_writer) = server.into_split();
    let (client_reader, _client_writer) = client.into_split();
    let mut client_reader = super::LimitedLineReader::new(client_reader);
    let mut known_peer = None;

    super::process_envelope(
        &network,
        &mut server_writer,
        remote_addr,
        &mut known_peer,
        GossipEnvelope::ChainBootstrapRequest,
    )
    .await
    .unwrap();

    let line = tokio::time::timeout(std::time::Duration::from_secs(1), client_reader.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let GossipEnvelope::ChainBootstrap(bootstrap) = super::parse_envelope(&line).unwrap() else {
        panic!("expected chain bootstrap");
    };
    assert_eq!(bootstrap.genesis_block, before.blocks[0]);
    assert_eq!(bootstrap.height, before.blocks.last().unwrap().height);
    assert_eq!(bootstrap.tip_hash, before.blocks.last().unwrap().hash);
    assert_eq!(network.inner.node.lock().await.chain_snapshot(), before);
    assert!(peers.lock().await.list().is_empty());
    assert!(known_peer.is_none());
}

#[tokio::test]
async fn hello_rejects_wrong_network_or_genesis_without_banning() {
    let alice = Wallet::from_seed("hello-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };

    let wrong_network = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: "other-network".to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: None,
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    assert!(
        super::process_hello(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            &mut None,
            wrong_network,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("wrong network")
    );

    let wrong_genesis = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: "not-local-genesis".to_string(),
        listen_addr: Some("127.0.0.1:9545".to_string()),
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    assert!(
        super::process_hello(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            &mut None,
            wrong_genesis,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("wrong genesis")
    );

    let wrong_protocol = ProtocolHello {
        protocol_version: PROTOCOL_VERSION + 1,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: Some("127.0.0.1:9545".to_string()),
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    assert!(
        super::process_hello(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            &mut None,
            wrong_protocol,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("unsupported protocol version")
    );

    assert!(network.inner.peers.lock().await.list().is_empty());
}

#[tokio::test]
async fn hello_records_remote_clock_observation() {
    let alice = Wallet::from_seed("hello-clock-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let remote_time_ms = crate::app::now_ms().saturating_add(60_000);
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: Some("127.0.0.1:9545".to_string()),
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: remote_time_ms,
    };

    let mut known_peer = None;
    super::process_hello(
        &network,
        "127.0.0.1:9545".parse().unwrap(),
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    let peers = network.inner.peers.lock().await.list();
    let peer = peers
        .iter()
        .find(|peer| peer.address == "127.0.0.1:9545")
        .unwrap();
    assert!(peer.last_clock_offset_ms.unwrap() > 30_000);
    assert_eq!(peer.last_clock_offset_accepted, Some(true));
}

#[tokio::test]
async fn hello_remembers_advertised_address_after_signed_session_and_dialback() {
    let alice = Wallet::from_seed("hello-dialback-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: Arc::clone(&node),
            peers: Arc::clone(&peers),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let remote_node_id = super::new_node_id();
    let remote_addr = spawn_hello_server(ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: None,
        node_id: Some(remote_node_id.clone()),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    })
    .await;
    let original_addr = spawn_verification_responder(remote_node_id.clone()).await;
    let stream = tokio::net::TcpStream::connect(original_addr).await.unwrap();
    let remote_socket = stream.peer_addr().unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = super::LimitedLineReader::new(reader);
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: Some(remote_addr.to_string()),
        node_id: Some(remote_node_id),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    let mut known_peer = None;

    super::process_hello_with_verification(
        &network,
        &mut writer,
        &mut reader,
        "test-original-peer",
        remote_socket,
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert_eq!(known_peer, Some(remote_addr.to_string()));
    let listed = peers.lock().await.list();
    let peer = listed
        .iter()
        .find(|peer| peer.address == remote_addr.to_string())
        .unwrap();
    assert_eq!(peer.direction, PeerDirection::Outbound);
    assert!(
        peers
            .lock()
            .await
            .addresses()
            .contains(&remote_addr.to_string())
    );
}

#[tokio::test]
async fn hello_ignores_advertised_address_when_connected_peer_cannot_sign_claimed_node_id() {
    let alice = Wallet::from_seed("hello-dialback-spoof-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: Arc::clone(&node),
            peers: Arc::clone(&peers),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let victim_node_id = super::new_node_id();
    let attacker_node_id = super::new_node_id();
    let remote_addr = spawn_hello_server(ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: None,
        node_id: Some(victim_node_id.clone()),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    })
    .await;
    let original_addr = spawn_verification_responder(attacker_node_id).await;
    let stream = tokio::net::TcpStream::connect(original_addr).await.unwrap();
    let remote_socket = stream.peer_addr().unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = super::LimitedLineReader::new(reader);
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: Some(remote_addr.to_string()),
        node_id: Some(victim_node_id),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    let mut known_peer = None;

    super::process_hello_with_verification(
        &network,
        &mut writer,
        &mut reader,
        "test-attacker-peer",
        remote_socket,
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert!(known_peer.is_none());
    assert!(
        !peers
            .lock()
            .await
            .addresses()
            .contains(&remote_addr.to_string())
    );
}

#[tokio::test]
async fn dialback_rejects_address_that_signs_with_different_node_id() {
    let alice = Wallet::from_seed("hello-dialback-mismatch-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: Arc::clone(&node),
            peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let honest_node_id = super::new_node_id();
    let claimed_node_id = super::new_node_id();
    let remote_addr = spawn_hello_server(ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: None,
        node_id: Some(honest_node_id),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    })
    .await;

    assert!(
        !super::verify_advertised_peer_node_id(
            &network,
            &remote_addr.to_string(),
            &claimed_node_id
        )
        .await
    );
}

#[tokio::test]
async fn inbound_verification_only_session_closes_after_response() {
    let alice = Wallet::from_seed("verification-only-close-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen_addr = listener.local_addr().unwrap();
    drop(listener);
    let network = super::GossipNetwork::start(node, peers, listen_addr, None, true)
        .await
        .unwrap();

    let stream = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = super::LimitedLineReader::new(reader);
    let hello_line = reader.read_line().await.unwrap().unwrap();
    let node_id = match super::parse_envelope(&hello_line).unwrap() {
        GossipEnvelope::Hello(hello) => hello.node_id.unwrap(),
        other => panic!("expected hello, got {other:?}"),
    };
    let nonce = super::new_verification_nonce();
    super::write_envelope(
        &mut writer,
        &GossipEnvelope::PeerVerificationChallenge {
            address: listen_addr.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await
    .unwrap();

    let response_line = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match super::parse_envelope(&response_line).unwrap() {
        GossipEnvelope::PeerVerificationResponse {
            address,
            nonce: response_nonce,
            node_id: response_node_id,
            signature,
        } => assert!(super::peer_verification_response_is_valid(
            &address,
            &response_nonce,
            &response_node_id,
            &signature,
            &listen_addr.to_string(),
            &nonce,
            &node_id,
        )),
        other => panic!("expected verification response, got {other:?}"),
    }

    let closed = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line())
        .await
        .unwrap()
        .unwrap();
    assert!(closed.is_none());
    network.set_accept_inbound(false).await.unwrap();
}

#[tokio::test]
async fn setup_placeholder_accepts_remote_genesis_and_adopts_bootstrap() {
    let local_wallet = Wallet::from_seed("setup-placeholder-local");
    let local_ledger = Ledger::new(BTreeMap::new(), 1);
    let local_node = Arc::new(tokio::sync::Mutex::new(NodeCore::from_ledger(
        local_wallet,
        local_ledger.clone(),
        0,
    )));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: local_node,
            peers: Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
                "iuna.jhx.app:9444".to_string(),
            ]))),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };

    let remote_wallet = Wallet::from_seed("setup-placeholder-remote");
    let remote_node = node(
        "remote",
        remote_wallet.clone(),
        allocations(std::slice::from_ref(&remote_wallet), 1_000),
    );
    let remote_genesis = remote_node.ledger().genesis_hash().to_string();
    let remote_bootstrap = remote_node.chain_bootstrap();
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: remote_genesis.clone(),
        listen_addr: Some("142.132.164.59:9444".to_string()),
        node_id: None,
        height: 5,
        tip_hash: "remote-tip".to_string(),
        time_ms: 1_000,
    };
    let mut known_peer = Some("iuna.jhx.app:9444".to_string());
    let peer_status = super::process_hello(
        &network,
        "142.132.164.59:51234".parse().unwrap(),
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert!(peer_status.request_bootstrap);
    assert!(!peer_status.push_bootstrap);
    assert_eq!(known_peer.as_deref(), Some("iuna.jhx.app:9444"));
    let listed = network.inner.peers.lock().await.list();
    assert_eq!(listed.len(), 1);
    let peer = listed
        .into_iter()
        .find(|peer| peer.address == "iuna.jhx.app:9444")
        .unwrap();
    assert_eq!(peer.misbehavior_score, 0);
    assert!(!peer.is_banned_at(crate::app::now_ms()));

    let adopted = super::validate_chain_bootstrap(remote_bootstrap, crate::app::now_ms())
        .await
        .unwrap();
    assert_eq!(adopted.genesis_hash(), remote_genesis);
    assert!(
        network
            .inner
            .node
            .lock()
            .await
            .import_verified_ledger(adopted)
            .unwrap()
    );
    assert_eq!(
        network.inner.node.lock().await.ledger().genesis_hash(),
        remote_genesis
    );
}

#[tokio::test]
async fn real_node_accepts_setup_placeholder_peer_and_pushes_bootstrap() {
    let wallet = Wallet::from_seed("setup-placeholder-peer-real-node");
    let node = Arc::new(tokio::sync::Mutex::new(node(
        "real",
        wallet.clone(),
        allocations(std::slice::from_ref(&wallet), 1_000),
    )));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: Arc::clone(&node),
            peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
            listen_addr: "127.0.0.1:9544".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let setup_ledger = Ledger::new(BTreeMap::new(), 1);
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: setup_ledger.genesis_hash().to_string(),
        listen_addr: Some("127.0.0.1:9545".to_string()),
        node_id: None,
        height: 0,
        tip_hash: setup_ledger.status().tip_hash,
        time_ms: 1_000,
    };

    let peer_status = super::process_hello(
        &network,
        "127.0.0.1:51234".parse().unwrap(),
        &mut None,
        hello,
    )
    .await
    .unwrap();

    assert!(!peer_status.request_bootstrap);
    assert!(peer_status.push_bootstrap);
    let payload = super::catchup_payload_for_peer(&node, &peer_status).await;
    assert!(matches!(
        payload.as_slice(),
        [GossipEnvelope::ChainBootstrap(_)]
    ));
}

#[tokio::test]
async fn lagging_peer_receives_block_pages_before_mempool() {
    let wallet = Wallet::from_seed("lagging-peer-blocks-before-mempool");
    let mut local = node(
        "lagging-peer-source",
        wallet.clone(),
        allocations(std::slice::from_ref(&wallet), 1_000),
    );
    let genesis_hash = local.ledger().genesis_hash().to_string();
    queue_plaintext_burn(&mut local, &wallet, 1);
    local.drain_outbox();
    local.mine_one_at(1).unwrap();
    local.drain_outbox();
    queue_plaintext_burn(&mut local, &wallet, 1);
    assert!(!local.ledger().pending().is_empty());
    let local = Arc::new(tokio::sync::Mutex::new(local));
    let peer_status = super::PeerStatus::new(0, genesis_hash);

    let payload = super::catchup_payload_for_peer(&local, &peer_status).await;

    assert!(matches!(
        payload.as_slice(),
        [GossipEnvelope::Blocks { .. }]
    ));
}

#[tokio::test]
async fn hello_ignores_private_advertised_listen_address() {
    let alice = Wallet::from_seed("hello-private-listen-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node: Arc::clone(&node),
            peers: Arc::clone(&peers),
            listen_addr: "0.0.0.0:9444".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let status = node.lock().await.ledger().status();
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
        listen_addr: Some("10.42.1.1:12138".to_string()),
        node_id: None,
        height: status.height,
        tip_hash: status.tip_hash,
        time_ms: 1_000,
    };

    let mut known_peer = None;
    super::process_hello(
        &network,
        "142.132.164.59:51234".parse().unwrap(),
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert!(known_peer.is_none());
    assert!(peers.lock().await.addresses().is_empty());
}

#[tokio::test]
async fn hello_ignores_loopback_alias_for_unspecified_self() {
    let alice = Wallet::from_seed("hello-self-alias-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::clone(&peers),
            listen_addr: "0.0.0.0:9545".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: Some("127.0.0.1:9545".to_string()),
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };

    super::process_hello(
        &network,
        "127.0.0.1:52144".parse().unwrap(),
        &mut None,
        hello,
    )
    .await
    .unwrap();

    assert_eq!(network.metrics().self_peer_rejections, 1);
    assert!(peers.lock().await.addresses().is_empty());
    let listed = peers.lock().await.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].direction, PeerDirection::Inbound);
}

#[tokio::test]
async fn hello_removes_outbound_peer_that_announces_self_address() {
    let alice = Wallet::from_seed("hello-self-outbound-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
        "10.42.1.1:16987".to_string(),
    ])));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::clone(&peers),
            listen_addr: "0.0.0.0:9444".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: Some("127.0.0.1:9444".to_string()),
        node_id: None,
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    let mut known_peer = Some("10.42.1.1:16987".to_string());

    super::process_hello(
        &network,
        "10.42.1.1:16987".parse().unwrap(),
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert_eq!(network.metrics().self_peer_rejections, 1);
    assert!(known_peer.is_none());
    assert!(peers.lock().await.addresses().is_empty());
}

#[tokio::test]
async fn hello_removes_outbound_peer_with_same_node_id() {
    let alice = Wallet::from_seed("hello-self-node-id-alice");
    let allocations = allocations(std::slice::from_ref(&alice), 1_000);
    let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
    let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
        "142.132.164.59:9444".to_string(),
    ])));
    let network = super::GossipNetwork {
        inner: Arc::new(super::GossipNetworkInner {
            node,
            peers: Arc::clone(&peers),
            listen_addr: "0.0.0.0:9444".parse().unwrap(),
            p2p_announce_addr: tokio::sync::Mutex::new(None),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(StdMutex::new(super::InboundConnectionLimiter::default())),
            metrics: super::P2pMetricsCounters::default(),
            sync_progress: StdMutex::new(super::SyncProgressState::default()),
        }),
    };
    let hello = ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        network_id: NETWORK_ID.to_string(),
        genesis_hash: network
            .inner
            .node
            .lock()
            .await
            .ledger()
            .genesis_hash()
            .to_string(),
        listen_addr: Some("0.0.0.0:9444".to_string()),
        node_id: Some(network.inner.node_id.clone()),
        height: 0,
        tip_hash: "tip".to_string(),
        time_ms: 1_000,
    };
    let mut known_peer = Some("142.132.164.59:9444".to_string());

    super::process_hello(
        &network,
        "142.132.164.59:52144".parse().unwrap(),
        &mut known_peer,
        hello,
    )
    .await
    .unwrap();

    assert_eq!(network.metrics().self_peer_rejections, 1);
    assert!(known_peer.is_none());
    assert!(peers.lock().await.addresses().is_empty());
}

async fn spawn_hello_server(hello: ProtocolHello) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let node_id = hello.node_id.clone();
        let (reader, mut writer) = stream.into_split();
        let line = serde_json::to_string(&GossipEnvelope::Hello(hello)).unwrap();
        let _ = writer.write_all(line.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;
        let Some(node_id) = node_id else {
            return;
        };
        let mut reader = super::LimitedLineReader::new(reader);
        let Ok(Some(line)) = reader.read_line().await else {
            return;
        };
        let Ok(GossipEnvelope::PeerVerificationChallenge { address, nonce }) =
            super::parse_envelope(&line)
        else {
            return;
        };
        let Some(response) =
            super::peer_verification_response_for_node_id(&node_id, &address, &nonce)
        else {
            return;
        };
        let line = serde_json::to_string(&response).unwrap();
        let _ = writer.write_all(line.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;
    });
    addr
}

async fn spawn_verification_responder(node_id: String) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (reader, mut writer) = stream.into_split();
        let mut reader = super::LimitedLineReader::new(reader);
        let Ok(Some(line)) = reader.read_line().await else {
            return;
        };
        let Ok(GossipEnvelope::PeerVerificationChallenge { address, nonce }) =
            super::parse_envelope(&line)
        else {
            return;
        };
        let Some(response) =
            super::peer_verification_response_for_node_id(&node_id, &address, &nonce)
        else {
            return;
        };
        let line = serde_json::to_string(&response).unwrap();
        let _ = writer.write_all(line.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;
    });
    addr
}
