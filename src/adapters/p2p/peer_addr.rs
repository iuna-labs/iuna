use std::{
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};

use crate::app::GossipEnvelope;

pub(super) fn next_reconnect_delay(current: Duration, max_delay: Duration) -> Duration {
    (current * 2).min(max_delay)
}

pub(super) fn peer_needs_snapshot(peer_height: u64, envelopes: &[GossipEnvelope]) -> bool {
    envelopes
        .iter()
        .filter_map(|envelope| match envelope {
            GossipEnvelope::Block(block) => Some(block.height),
            GossipEnvelope::Inventory { blocks, .. } => {
                blocks.iter().map(|block| block.height).min()
            }
            _ => None,
        })
        .min()
        .is_some_and(|first_block_height| peer_height + 1 < first_block_height)
}

pub(super) fn reachable_advertised_addr(
    advertised_addr: SocketAddr,
    remote_addr: SocketAddr,
) -> SocketAddr {
    let mut reachable_addr = advertised_addr;
    if reachable_addr.ip().is_unspecified() {
        reachable_addr.set_ip(remote_addr.ip());
    }
    reachable_addr
}

pub(super) fn normalize_advertised_peer(address: &str, remote_addr: SocketAddr) -> Result<String> {
    let advertised_addr = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid announced peer address {address}"))?;
    Ok(reachable_advertised_addr(advertised_addr, remote_addr).to_string())
}

pub(super) fn peer_list_address_is_discoverable(
    address: &str,
    remote_addr: SocketAddr,
) -> Result<bool> {
    let candidate = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid peer-list address {address}"))?;
    Ok(socket_addr_is_discoverable(candidate, remote_addr))
}

pub(super) fn advertised_peer_is_discoverable(
    address: &str,
    remote_addr: SocketAddr,
) -> Result<bool> {
    let candidate = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid announced peer address {address}"))?;
    Ok(socket_addr_is_discoverable(candidate, remote_addr))
}

fn socket_addr_is_discoverable(candidate: SocketAddr, remote_addr: SocketAddr) -> bool {
    if candidate.ip().is_loopback() {
        return remote_addr.ip().is_loopback();
    }
    ip_is_publicly_discoverable(candidate.ip())
}

fn ip_is_publicly_discoverable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || [a, b, c, d] == [255, 255, 255, 255])
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xff00) == 0xff00)
        }
    }
}

pub(super) fn is_self_peer_address_for(
    address: &str,
    listen_addr: SocketAddr,
    advertised_addr: Option<SocketAddr>,
) -> bool {
    address.parse::<SocketAddr>().is_ok_and(|candidate| {
        is_self_socket_addr(candidate, listen_addr)
            || advertised_addr.is_some_and(|addr| is_self_socket_addr(candidate, addr))
    })
}

fn is_self_socket_addr(candidate: SocketAddr, listen_addr: SocketAddr) -> bool {
    if candidate == listen_addr {
        return true;
    }
    if candidate.port() != listen_addr.port() {
        return false;
    }

    let candidate_ip = candidate.ip();
    let listen_ip = listen_addr.ip();
    if listen_ip.is_unspecified() {
        return candidate_ip.is_unspecified() || candidate_ip.is_loopback();
    }
    if candidate_ip.is_unspecified() {
        return listen_ip.is_loopback();
    }
    false
}

pub(super) fn is_quiet_disconnect(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionAborted
            )
        })
    })
}

pub(super) fn is_possible_fork_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("does not extend local tip")
        || message.contains("conflicts with local chain")
        || message.contains("expected block height")
}

pub(super) fn inbound_error_counts_as_misbehavior(message: &str) -> bool {
    !message.contains("block timestamp is too far in the future")
        && !message.contains("block timestamp is before finalizer rank")
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use crate::{
        app::GossipEnvelope,
        domain::{Block, FinalizerMode, RevealBundleSection},
    };

    use super::{is_self_peer_address_for, peer_needs_snapshot, reachable_advertised_addr};

    #[test]
    fn unspecified_announced_ip_uses_remote_ip_with_announced_port() {
        let advertised: SocketAddr = "0.0.0.0:9445".parse().unwrap();
        let remote: SocketAddr = "203.0.113.10:52144".parse().unwrap();

        assert_eq!(
            reachable_advertised_addr(advertised, remote).to_string(),
            "203.0.113.10:9445"
        );
    }

    #[test]
    fn explicit_announced_ip_is_kept() {
        let advertised: SocketAddr = "127.0.0.1:9445".parse().unwrap();
        let remote: SocketAddr = "127.0.0.1:52144".parse().unwrap();

        assert_eq!(
            reachable_advertised_addr(advertised, remote).to_string(),
            "127.0.0.1:9445"
        );
    }

    #[test]
    fn loopback_peer_on_unspecified_listen_port_is_self() {
        let listen_addr: SocketAddr = "0.0.0.0:9545".parse().unwrap();

        assert!(is_self_peer_address_for(
            "127.0.0.1:9545",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(is_self_peer_address_for(
            "0.0.0.0:9545",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(!is_self_peer_address_for(
            "127.0.0.1:9546",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(!is_self_peer_address_for(
            "203.0.113.10:9545",
            listen_addr,
            Some(listen_addr)
        ));
    }

    #[test]
    fn peer_needs_snapshot_when_block_gossip_skips_a_height() {
        let block = Block {
            height: 10,
            prev_hash: "prev".to_string(),
            timestamp_ms: 1,
            miner: "miner".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 100,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            blinded_transactions: Vec::new(),
            reveal_bundle_section: RevealBundleSection::default(),
            transactions: Vec::new(),
            hash: "hash".to_string(),
        };

        assert!(peer_needs_snapshot(
            8,
            &[GossipEnvelope::Block(block.clone())]
        ));
        assert!(!peer_needs_snapshot(9, &[GossipEnvelope::Block(block)]));
        assert!(!peer_needs_snapshot(
            8,
            &[GossipEnvelope::PeerAnnouncement {
                address: "127.0.0.1:9444".to_string(),
                node_id: Some("peer-node".to_string()),
            }]
        ));
    }
}
