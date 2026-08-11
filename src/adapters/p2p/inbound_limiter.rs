use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
};

use super::{
    INBOUND_ACCEPT_RATE_WINDOW_MS, MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW, MAX_INBOUND_SESSIONS,
    MAX_INBOUND_SESSIONS_PER_IP,
};

#[derive(Default)]
pub(super) struct InboundConnectionLimiter {
    active: usize,
    peers: BTreeMap<IpAddr, InboundPeerLimit>,
}

#[derive(Default)]
struct InboundPeerLimit {
    active: usize,
    accepted_at_ms: VecDeque<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InboundSessionRejection {
    GlobalActive,
    PeerActive,
    PeerRate,
}

impl InboundSessionRejection {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::GlobalActive => "global active inbound session limit",
            Self::PeerActive => "per-IP active inbound session limit",
            Self::PeerRate => "per-IP inbound accept rate limit",
        }
    }
}

pub(super) struct InboundSessionPermit {
    pub(super) limiter: Arc<StdMutex<InboundConnectionLimiter>>,
    pub(super) ip: IpAddr,
}

impl Drop for InboundSessionPermit {
    fn drop(&mut self) {
        if let Ok(mut limiter) = self.limiter.lock() {
            limiter.release(self.ip);
        }
    }
}

impl InboundConnectionLimiter {
    pub(super) fn try_acquire(
        &mut self,
        ip: IpAddr,
        now_ms: u64,
    ) -> std::result::Result<(), InboundSessionRejection> {
        self.prune_stale_accepts(now_ms);
        if self.active >= MAX_INBOUND_SESSIONS {
            return Err(InboundSessionRejection::GlobalActive);
        }

        let peer = self.peers.entry(ip).or_default();
        prune_peer_accepts(peer, now_ms);
        if peer.active >= MAX_INBOUND_SESSIONS_PER_IP {
            return Err(InboundSessionRejection::PeerActive);
        }
        if peer.accepted_at_ms.len() >= MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW {
            return Err(InboundSessionRejection::PeerRate);
        }

        peer.active += 1;
        peer.accepted_at_ms.push_back(now_ms);
        self.active += 1;
        Ok(())
    }

    pub(super) fn release(&mut self, ip: IpAddr) {
        if self.active > 0 {
            self.active -= 1;
        }
        if let Some(peer) = self.peers.get_mut(&ip) {
            if peer.active > 0 {
                peer.active -= 1;
            }
        }
    }

    fn prune_stale_accepts(&mut self, now_ms: u64) {
        self.peers.retain(|_, peer| {
            prune_peer_accepts(peer, now_ms);
            peer.active > 0 || !peer.accepted_at_ms.is_empty()
        });
    }
}

fn prune_peer_accepts(peer: &mut InboundPeerLimit, now_ms: u64) {
    while peer.accepted_at_ms.front().is_some_and(|accepted_ms| {
        now_ms.saturating_sub(*accepted_ms) >= INBOUND_ACCEPT_RATE_WINDOW_MS
    }) {
        peer.accepted_at_ms.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        INBOUND_ACCEPT_RATE_WINDOW_MS, MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW, MAX_INBOUND_SESSIONS,
        MAX_INBOUND_SESSIONS_PER_IP,
    };
    use super::{InboundConnectionLimiter, InboundSessionRejection};

    #[test]
    fn inbound_limiter_enforces_per_ip_active_limit() {
        let ip = "203.0.113.10".parse().unwrap();
        let mut limiter = InboundConnectionLimiter::default();
        for _ in 0..MAX_INBOUND_SESSIONS_PER_IP {
            limiter.try_acquire(ip, 1_000).unwrap();
        }

        assert_eq!(
            limiter.try_acquire(ip, 1_000).unwrap_err(),
            InboundSessionRejection::PeerActive
        );

        limiter.release(ip);
        limiter.try_acquire(ip, 1_000).unwrap();
    }

    #[test]
    fn inbound_limiter_enforces_global_active_limit() {
        let mut limiter = InboundConnectionLimiter::default();
        for index in 0..MAX_INBOUND_SESSIONS {
            let ip = format!("198.51.100.{index}").parse().unwrap();
            limiter.try_acquire(ip, 1_000).unwrap();
        }

        assert_eq!(
            limiter
                .try_acquire("203.0.113.200".parse().unwrap(), 1_000)
                .unwrap_err(),
            InboundSessionRejection::GlobalActive
        );
    }

    #[test]
    fn inbound_limiter_enforces_per_ip_accept_rate() {
        let ip = "203.0.113.20".parse().unwrap();
        let mut limiter = InboundConnectionLimiter::default();
        for _ in 0..MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW {
            limiter.try_acquire(ip, 1_000).unwrap();
            limiter.release(ip);
        }

        assert_eq!(
            limiter.try_acquire(ip, 1_000).unwrap_err(),
            InboundSessionRejection::PeerRate
        );
        limiter
            .try_acquire(ip, 1_000 + INBOUND_ACCEPT_RATE_WINDOW_MS)
            .unwrap();
    }
}
