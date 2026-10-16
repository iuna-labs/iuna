use std::{
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};

use crate::domain::{ValidationError, error_has_validation};

use super::SyncError;

pub(super) fn next_reconnect_delay(current: Duration, max_delay: Duration) -> Duration {
    (current * 2).min(max_delay)
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
            if let Some(ip) = embedded_ipv4(ip) {
                return ip_is_publicly_discoverable(IpAddr::V4(ip));
            }
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xff00) == 0xff00)
        }
    }
}

fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(ip) = ip.to_ipv4_mapped() {
        return Some(ip);
    }

    let segments = ip.segments();
    if segments[..6] != [0; 6] {
        return None;
    }

    let [a, b] = segments[6].to_be_bytes();
    let [c, d] = segments[7].to_be_bytes();
    Some(Ipv4Addr::new(a, b, c, d))
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
    error_has_validation(error, ValidationError::requests_fork_recovery)
        || error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<SyncError>(),
                Some(SyncError::BlockPageHasNoCommonAncestor)
            )
        })
}

pub(super) fn inbound_error_counts_as_misbehavior(error: &anyhow::Error) -> bool {
    !is_possible_fork_error(error)
        && !error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<SyncError>(),
                Some(SyncError::ForkValidationThrottled | SyncError::ForkHistoryUnavailable)
            )
        })
        && !error_has_validation(error, |kind| {
            kind.is_fork_relative()
                || kind.is_temporal()
                || kind == ValidationError::MempoolConflict
        })
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use anyhow::anyhow;

    use crate::domain::ValidationError;

    use super::super::SyncError;

    use super::{
        inbound_error_counts_as_misbehavior, ip_is_publicly_discoverable, is_possible_fork_error,
    };

    #[test]
    fn embedded_ipv4_addresses_use_the_ipv4_discoverability_filter() {
        for address in [
            "::ffff:127.0.0.1",
            "::ffff:10.42.0.1",
            "::ffff:169.254.169.254",
            "::ffff:192.168.1.1",
            "::127.0.0.1",
            "::10.42.0.1",
            "::169.254.169.254",
            "::192.168.1.1",
        ] {
            assert!(
                !ip_is_publicly_discoverable(address.parse::<IpAddr>().unwrap()),
                "{address} must not be publicly discoverable"
            );
        }

        for address in ["::ffff:8.8.8.8", "::8.8.8.8"] {
            assert!(
                ip_is_publicly_discoverable(address.parse::<IpAddr>().unwrap()),
                "{address} should follow the public IPv4 address policy"
            );
        }
    }

    #[test]
    fn future_and_unopened_rank_slot_errors_are_temporal_not_misbehavior() {
        assert!(!inbound_error_counts_as_misbehavior(&anyhow::Error::new(
            ValidationError::BlockTimestampTooFarInFuture
        )));
        assert!(!inbound_error_counts_as_misbehavior(&anyhow::Error::new(
            ValidationError::BlockBeforeFinalizerRankSlot {
                rank: 1,
                min_timestamp: 123,
            }
        )));
        assert!(inbound_error_counts_as_misbehavior(&anyhow!(
            "block hash is invalid"
        )));
    }

    #[test]
    fn local_fork_validation_conditions_are_not_peer_misbehavior() {
        for condition in [
            SyncError::ForkValidationThrottled,
            SyncError::ForkHistoryUnavailable,
        ] {
            let error = anyhow::Error::new(condition).context("block batch");
            assert!(!inbound_error_counts_as_misbehavior(&error));
            assert!(!is_possible_fork_error(&error));
        }
    }

    #[test]
    fn missing_block_page_ancestor_triggers_fork_recovery_without_peer_penalty() {
        let error =
            anyhow::Error::new(SyncError::BlockPageHasNoCommonAncestor).context("block batch");

        assert!(is_possible_fork_error(&error));
        assert!(!inbound_error_counts_as_misbehavior(&error));
    }

    #[test]
    fn fork_scoped_gossip_errors_do_not_penalize_peers() {
        for kind in [
            ValidationError::BurnBundleParentMismatch,
            ValidationError::BurnAnchorOutsidePendingWindow,
            ValidationError::MineAnchorNotOnChain,
            ValidationError::MempoolConflict,
        ] {
            let error = anyhow::Error::new(kind);
            assert!(!inbound_error_counts_as_misbehavior(&error));
            assert!(!is_possible_fork_error(&error));
        }

        assert!(inbound_error_counts_as_misbehavior(&anyhow!(
            "burn bundle signature is invalid"
        )));
    }

    #[test]
    fn matching_text_without_a_typed_error_is_not_trusted() {
        let error = anyhow!("burn bundle parent hash is invalid");

        assert!(inbound_error_counts_as_misbehavior(&error));
        assert!(!is_possible_fork_error(&error));
    }
}
