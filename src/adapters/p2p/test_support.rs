use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use crate::{
    app::{NodeCore, PeerBook},
    domain::{Amount, GenesisBurn, Ledger, Transaction, Wallet},
};

use super::{GossipNetwork, GossipNetworkInner, InboundConnectionLimiter, P2pMetricsCounters};

pub(super) fn node(
    _network_key: &str,
    wallet: Wallet,
    allocations: BTreeMap<String, Amount>,
) -> NodeCore {
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), 1)],
        25,
    )
    .unwrap();
    NodeCore::from_ledger(wallet, ledger, 0)
}

pub(super) fn queue_plaintext_burn(
    node: &mut NodeCore,
    wallet: &Wallet,
    amount: Amount,
) -> Transaction {
    let tx = node.ledger().build_burn(wallet, amount, 1).unwrap();
    node.receive_transaction(tx.clone()).unwrap();
    tx
}

pub(super) fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
    wallets
        .iter()
        .map(|wallet| (wallet.address().to_string(), amount))
        .collect()
}

pub(super) fn gossip_network(
    node: Arc<tokio::sync::Mutex<NodeCore>>,
    peers: Arc<tokio::sync::Mutex<PeerBook>>,
    listen_addr: SocketAddr,
    p2p_announce_addr: Option<SocketAddr>,
) -> GossipNetwork {
    GossipNetwork {
        inner: Arc::new(GossipNetworkInner {
            node,
            peers,
            listen_addr,
            p2p_announce_addr: tokio::sync::Mutex::new(p2p_announce_addr),
            node_id: super::new_node_id(),
            accept_task: tokio::sync::Mutex::new(None),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            inbound_limiter: Arc::new(std::sync::Mutex::new(InboundConnectionLimiter::default())),
            metrics: P2pMetricsCounters::default(),
            sync_progress: std::sync::Mutex::new(super::SyncProgressState::default()),
        }),
    }
}
