use std::{collections::BTreeMap, net::SocketAddr};

use anyhow::Result;
use tokio::{
    net::{TcpStream, tcp::OwnedWriteHalf},
    time::timeout,
};

use super::identity::{
    new_verification_nonce, peer_verification_response, peer_verification_response_is_valid,
};
use super::line_codec::{LimitedLineReader, parse_envelope, read_session_envelope};
use super::metrics::P2pMetricsCounters;
use super::peer_addr::{advertised_peer_is_discoverable, normalize_advertised_peer};
use super::{
    CONNECT_TIMEOUT, GossipNetwork, HANDSHAKE_TIMEOUT, MAX_PEER_VERIFICATION_ENVELOPES, PeerStatus,
    write_envelope,
};
use crate::{
    app::{
        GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION, PeerDirection, ProtocolHello,
        debug_logging_enabled, now_ms, validate_protocol_capabilities,
        validate_transaction_v2_peer_capability,
    },
    domain::Ledger,
};

pub(super) struct PeerVerificationSession<'a> {
    pub(super) writer: &'a mut OwnedWriteHalf,
    pub(super) reader: &'a mut LimitedLineReader<tokio::net::tcp::OwnedReadHalf>,
    pub(super) connection_label: &'a str,
}

pub(super) async fn record_peer_status(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    peer_status: &PeerStatus,
) {
    let local_receive_time_ms = now_ms();
    if let Some(peer) = known_peer {
        let mut peers = network.inner.peers.lock().await;
        peers.record_status(peer, peer_status.height, peer_status.tip_hash.clone());
        peers.record_clock_observation(
            peer,
            PeerDirection::Outbound,
            peer_status.time_ms,
            local_receive_time_ms,
        );
    } else {
        let peer = remote_addr.to_string();
        let mut peers = network.inner.peers.lock().await;
        peers.record_clock_observation(
            &peer,
            PeerDirection::Inbound,
            peer_status.time_ms,
            local_receive_time_ms,
        );
        peers.record_received(&peer, 1);
    }
}

async fn record_peer_hello(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    hello: ProtocolHello,
) {
    let (peer, direction) = match known_peer {
        Some(peer) => (peer.clone(), PeerDirection::Outbound),
        None => (remote_addr.to_string(), PeerDirection::Inbound),
    };
    network
        .inner
        .peers
        .lock()
        .await
        .record_hello(&peer, direction, hello);
}

pub(super) async fn process_hello(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    process_hello_inner(network, None, remote_addr, known_peer, hello).await
}

pub(super) async fn process_hello_with_verification(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    reader: &mut LimitedLineReader<tokio::net::tcp::OwnedReadHalf>,
    connection_label: &str,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    let mut verification_session = PeerVerificationSession {
        writer,
        reader,
        connection_label,
    };
    process_hello_inner(
        network,
        Some(&mut verification_session),
        remote_addr,
        known_peer,
        hello,
    )
    .await
}

async fn process_hello_inner(
    network: &GossipNetwork,
    mut verification_session: Option<&mut PeerVerificationSession<'_>>,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    if hello.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported protocol version {}; expected {}",
            hello.protocol_version,
            PROTOCOL_VERSION
        );
    }
    validate_protocol_capabilities(&hello.capabilities)?;
    if hello.network_id != NETWORK_ID {
        anyhow::bail!(
            "wrong network {}; expected {}",
            hello.network_id,
            NETWORK_ID
        );
    }
    if hello
        .node_id
        .as_deref()
        .is_some_and(|node_id| node_id == network.inner.node_id)
    {
        P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
        forget_stale_self_peer(network, known_peer).await;
        return Ok(PeerStatus::rejected(
            hello.height,
            hello.tip_hash,
            hello.time_ms,
        ));
    }
    let (local_genesis, local_accepts_remote_genesis, local_height) = {
        let node = network.inner.node.lock().await;
        (
            node.ledger().genesis_hash().to_string(),
            node.ledger().is_setup_placeholder(),
            node.ledger().height(),
        )
    };
    validate_transaction_v2_peer_capability(&hello.capabilities, local_height, hello.height)?;
    let genesis_mismatch = hello.genesis_hash != local_genesis;
    let remote_is_setup_placeholder =
        hello.height == 0 && hello.genesis_hash == setup_placeholder_genesis_hash();
    let request_bootstrap = genesis_mismatch && local_accepts_remote_genesis;
    if genesis_mismatch && !local_accepts_remote_genesis && !remote_is_setup_placeholder {
        anyhow::bail!(
            "wrong genesis {}; expected {local_genesis}",
            hello.genesis_hash
        );
    }

    let remote_node_id = hello.node_id.clone();
    let mut reject_session = false;
    if let Some(listen_addr) = &hello.listen_addr {
        let peer = normalize_advertised_peer(listen_addr, remote_addr)?;
        if network.is_self_peer(&peer).await {
            P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
            forget_stale_self_peer(network, known_peer).await;
            reject_session = true;
        } else {
            let verified = match verification_session.as_mut() {
                Some(session) => {
                    remember_verified_advertised_peer(
                        network,
                        session,
                        remote_addr,
                        known_peer,
                        peer.clone(),
                        remote_node_id.as_deref(),
                    )
                    .await?
                }
                None => false,
            };
            if !verified && debug_logging_enabled() {
                eprintln!(
                    "p2p advertised address {peer} ignored because ownership was not verified"
                );
            }
        }
    }
    record_peer_status(
        network,
        known_peer,
        remote_addr,
        &PeerStatus::with_time(hello.height, hello.tip_hash.clone(), hello.time_ms),
    )
    .await;
    record_peer_hello(network, known_peer, remote_addr, hello.clone()).await;
    let mut status = if request_bootstrap {
        PeerStatus::with_bootstrap_request(hello.height, hello.tip_hash, hello.time_ms)
    } else {
        PeerStatus::with_time(hello.height, hello.tip_hash, hello.time_ms)
    };
    status.reject_session = reject_session;
    Ok(status)
}

fn setup_placeholder_genesis_hash() -> String {
    Ledger::new(BTreeMap::new(), 1).genesis_hash().to_string()
}

async fn remember_verified_advertised_peer(
    network: &GossipNetwork,
    session: &mut PeerVerificationSession<'_>,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    peer: String,
    expected_node_id: Option<&str>,
) -> Result<bool> {
    if !advertised_peer_is_discoverable(&peer, remote_addr)? {
        return Ok(false);
    }
    if known_peer.as_deref() != Some(peer.as_str()) {
        let Some(expected_node_id) = expected_node_id else {
            return Ok(false);
        };
        if !verify_connected_peer_node_id(network, session, &peer, expected_node_id).await? {
            return Ok(false);
        }
        if !verify_advertised_peer_node_id(network, &peer, expected_node_id).await {
            return Ok(false);
        }
    }
    remember_discoverable_advertised_peer(network, remote_addr, known_peer, peer).await
}

async fn verify_connected_peer_node_id(
    network: &GossipNetwork,
    session: &mut PeerVerificationSession<'_>,
    peer: &str,
    expected_node_id: &str,
) -> Result<bool> {
    let nonce = new_verification_nonce();
    write_envelope(
        session.writer,
        &GossipEnvelope::PeerVerificationChallenge {
            address: peer.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await?;

    for _ in 0..MAX_PEER_VERIFICATION_ENVELOPES {
        let envelope = match timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(network, session.connection_label, session.reader),
        )
        .await
        {
            Ok(Ok(Some(envelope))) => envelope,
            Ok(Ok(None)) | Err(_) => return Ok(false),
            Ok(Err(error)) => return Err(error),
        };
        match envelope {
            GossipEnvelope::PeerVerificationResponse {
                address,
                nonce: response_nonce,
                node_id,
                signature,
            } => {
                return Ok(peer_verification_response_is_valid(
                    &address,
                    &response_nonce,
                    &node_id,
                    &signature,
                    peer,
                    &nonce,
                    expected_node_id,
                ));
            }
            GossipEnvelope::PeerVerificationChallenge { address, nonce } => {
                if let Some(response) = peer_verification_response(network, &address, &nonce) {
                    write_envelope(session.writer, &response).await?;
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

pub(super) async fn verify_advertised_peer_node_id(
    network: &GossipNetwork,
    peer: &str,
    expected_node_id: &str,
) -> bool {
    let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(peer)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            if debug_logging_enabled() {
                eprintln!("p2p announced address {peer} failed verification: {error}");
            }
            return false;
        }
        Err(_) => {
            if debug_logging_enabled() {
                eprintln!("p2p announced address {peer} failed verification: timeout");
            }
            return false;
        }
    };
    let (reader, mut writer) = stream.into_split();
    let mut reader = LimitedLineReader::new(reader);
    let line = match timeout(HANDSHAKE_TIMEOUT, reader.read_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return false,
        Ok(Err(error)) => {
            if debug_logging_enabled() {
                eprintln!(
                    "p2p announced address {peer} sent invalid verification hello: {error:#}"
                );
            }
            return false;
        }
        Err(_) => return false,
    };
    let hello = match parse_envelope(&line) {
        Ok(GossipEnvelope::Hello(hello)) => hello,
        Ok(_) | Err(_) => return false,
    };

    if !advertised_peer_hello_is_compatible(network, &hello).await
        || hello.node_id.as_deref() != Some(expected_node_id)
    {
        return false;
    }

    let nonce = new_verification_nonce();
    if write_envelope(
        &mut writer,
        &GossipEnvelope::PeerVerificationChallenge {
            address: peer.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return false;
    }
    for _ in 0..MAX_PEER_VERIFICATION_ENVELOPES {
        let line = match timeout(HANDSHAKE_TIMEOUT, reader.read_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return false,
        };
        let envelope = match parse_envelope(&line) {
            Ok(envelope) => envelope,
            Err(_) => return false,
        };
        if let GossipEnvelope::PeerVerificationResponse {
            address,
            nonce: response_nonce,
            node_id,
            signature,
        } = envelope
        {
            return peer_verification_response_is_valid(
                &address,
                &response_nonce,
                &node_id,
                &signature,
                peer,
                &nonce,
                expected_node_id,
            );
        }
    }
    false
}

async fn advertised_peer_hello_is_compatible(
    network: &GossipNetwork,
    hello: &ProtocolHello,
) -> bool {
    if hello.protocol_version != PROTOCOL_VERSION
        || hello.network_id != NETWORK_ID
        || validate_protocol_capabilities(&hello.capabilities).is_err()
    {
        return false;
    }
    let (local_genesis, local_accepts_remote_genesis, local_height) = {
        let node = network.inner.node.lock().await;
        (
            node.ledger().genesis_hash().to_string(),
            node.ledger().is_setup_placeholder(),
            node.ledger().height(),
        )
    };
    if validate_transaction_v2_peer_capability(&hello.capabilities, local_height, hello.height)
        .is_err()
    {
        return false;
    }
    let remote_is_setup_placeholder =
        hello.height == 0 && hello.genesis_hash == setup_placeholder_genesis_hash();
    hello.genesis_hash == local_genesis
        || local_accepts_remote_genesis
        || remote_is_setup_placeholder
}

pub(super) async fn remember_discoverable_advertised_peer(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    peer: String,
) -> Result<bool> {
    if !advertised_peer_is_discoverable(&peer, remote_addr)? {
        return Ok(false);
    }
    if let Some(previous_peer) = known_peer.as_deref() {
        network
            .inner
            .peers
            .lock()
            .await
            .replace_peer_address(previous_peer, peer.clone());
    } else {
        network
            .inner
            .peers
            .lock()
            .await
            .add_discovered_peer(peer.clone());
    }
    *known_peer = Some(peer);
    Ok(true)
}

pub(super) async fn forget_stale_self_peer(
    network: &GossipNetwork,
    known_peer: &mut Option<String>,
) {
    if let Some(previous_peer) = known_peer.take() {
        network.inner.peers.lock().await.remove_peer(&previous_peer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        app::{PeerBook, PeerDirection},
        domain::Wallet,
    };

    use super::super::{
        PeerStatus,
        test_support::{allocations, gossip_network, node},
    };
    use super::{
        forget_stale_self_peer, record_peer_status, remember_discoverable_advertised_peer,
    };

    #[tokio::test]
    async fn inbound_announced_address_replaces_gateway_address_for_ui() {
        let alice = Wallet::from_seed("hello-public-announced-inbound-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9444".parse().unwrap(),
            None,
        );
        let mut known_peer = None;

        let remembered = remember_discoverable_advertised_peer(
            &network,
            "10.42.0.1:51234".parse().unwrap(),
            &mut known_peer,
            "142.132.164.59:9444".to_string(),
        )
        .await
        .unwrap();
        record_peer_status(
            &network,
            &known_peer,
            "10.42.0.1:51234".parse().unwrap(),
            &PeerStatus::with_time(7, "tip".to_string(), 1_000),
        )
        .await;

        assert!(remembered);
        assert_eq!(known_peer.as_deref(), Some("142.132.164.59:9444"));
        let listed = peers.lock().await.list();
        assert_eq!(listed.len(), 1);
        let peer = &listed[0];
        assert_eq!(peer.address, "142.132.164.59:9444");
        assert_eq!(peer.direction, PeerDirection::Outbound);
        assert_eq!(peer.last_known_height, Some(7));
        assert_eq!(peer.messages_received, 0);

        let repeated = remember_discoverable_advertised_peer(
            &network,
            "10.42.0.1:51234".parse().unwrap(),
            &mut known_peer,
            "142.132.164.59:9444".to_string(),
        )
        .await
        .unwrap();

        assert!(repeated);
        assert_eq!(
            peers.lock().await.list()[0].direction,
            PeerDirection::Outbound
        );
    }

    #[tokio::test]
    async fn inbound_status_does_not_create_outbound_ephemeral_peer() {
        let alice = Wallet::from_seed("inbound-status-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "127.0.0.1:9544".parse().unwrap(),
            None,
        );

        record_peer_status(
            &network,
            &None,
            "127.0.0.1:51729".parse().unwrap(),
            &PeerStatus::new(4, "tip".to_string()),
        )
        .await;

        let peers = peers.lock().await;
        assert!(peers.addresses().is_empty());
        let listed = peers.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].direction, PeerDirection::Inbound);
    }

    #[tokio::test]
    async fn peer_announcement_ignores_private_ephemeral_address() {
        let alice = Wallet::from_seed("px-private-announcement-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9444".parse().unwrap(),
            None,
        );
        let mut known_peer = None;

        let remembered = remember_discoverable_advertised_peer(
            &network,
            "142.132.164.59:51234".parse().unwrap(),
            &mut known_peer,
            "10.42.1.1:10091".to_string(),
        )
        .await
        .unwrap();

        assert!(!remembered);
        assert!(known_peer.is_none());
        assert!(peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn peer_announcement_removes_outbound_peer_that_announces_self_address() {
        let alice = Wallet::from_seed("px-self-announcement-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "10.42.1.1:30508".to_string(),
        ])));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9444".parse().unwrap(),
            None,
        );
        let mut known_peer = Some("10.42.1.1:30508".to_string());

        forget_stale_self_peer(&network, &mut known_peer).await;

        assert_eq!(known_peer, None);
        assert!(peers.lock().await.addresses().is_empty());
    }
}
