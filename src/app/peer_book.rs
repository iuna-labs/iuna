use std::{collections::BTreeMap, net::SocketAddr};

use serde::{Deserialize, Serialize};

use super::{
    PEER_CLOCK_OFFSET_ACCEPTANCE_MS, PEER_CLOCK_OFFSET_STALE_MS, PEER_MISBEHAVIOR_BAN_MS,
    PEER_MISBEHAVIOR_BAN_SCORE, ProtocolHello, now_ms,
};

pub const MAX_DISCOVERED_PEERS: usize = 256;
pub const MAX_DISCOVERED_PEERS_PER_IP: usize = 4;
pub const MAX_DISCOVERED_PEERS_PER_IPV4_PREFIX: usize = 16;
pub const MAX_DISCOVERED_PEERS_PER_IPV6_PREFIX: usize = 16;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerBook {
    peers: BTreeMap<String, PeerInfo>,
}

impl PeerBook {
    pub fn has_good_connection_at(&self, now_ms: u64) -> bool {
        self.peers
            .values()
            .any(|peer| peer.is_good_connection_at(now_ms))
    }

    pub fn has_good_connection_for_chain_at(
        &self,
        height: u64,
        tip_hash: &str,
        now_ms: u64,
    ) -> bool {
        self.peers
            .values()
            .any(|peer| peer.is_good_connection_for_chain_at(height, tip_hash, now_ms))
    }

    pub fn from_addresses(addresses: Vec<String>) -> Self {
        let mut book = Self::default();
        for address in addresses {
            book.add_peer(address);
        }
        book
    }

    pub fn add_peer(&mut self, address: impl Into<String>) {
        let address = address.into();
        let peer = self
            .peers
            .entry(address.clone())
            .or_insert_with(|| PeerInfo::new(address, PeerDirection::Outbound));
        if peer.direction != PeerDirection::Outbound {
            peer.direction = PeerDirection::Outbound;
        }
    }

    pub fn add_discovered_peer(&mut self, address: impl Into<String>) -> bool {
        let address = address.into();
        if let Some(direction) = self.peers.get(&address).map(|peer| peer.direction.clone()) {
            if direction == PeerDirection::Inbound && !self.discovered_peer_has_room(&address) {
                return false;
            }
            if let Some(peer) = self.peers.get_mut(&address) {
                if peer.direction == PeerDirection::Inbound {
                    peer.direction = PeerDirection::Discovered;
                }
                peer.last_contact_ms = peer.last_contact_ms.or_else(|| Some(now_ms()));
            }
            return true;
        }
        if !self.discovered_peer_has_room(&address) {
            return false;
        }
        let mut peer = PeerInfo::new(address.clone(), PeerDirection::Discovered);
        peer.last_contact_ms = Some(now_ms());
        self.peers.insert(address, peer);
        true
    }

    fn discovered_peer_has_room(&self, address: &str) -> bool {
        if self.discovered_peer_count() >= MAX_DISCOVERED_PEERS {
            return false;
        }
        let Ok(candidate) = address.parse::<SocketAddr>() else {
            return false;
        };
        let candidate_ip = candidate.ip();
        let mut same_ip = 0usize;
        let mut same_group = 0usize;
        for peer in self
            .peers
            .values()
            .filter(|peer| peer.direction == PeerDirection::Discovered)
        {
            let Ok(existing) = peer.address.parse::<SocketAddr>() else {
                continue;
            };
            let existing_ip = existing.ip();
            if existing_ip == candidate_ip {
                same_ip += 1;
            }
            if same_discovery_group(existing, candidate) {
                same_group += 1;
            }
        }
        if same_ip >= MAX_DISCOVERED_PEERS_PER_IP {
            return false;
        }
        match candidate {
            SocketAddr::V4(_) => same_group < MAX_DISCOVERED_PEERS_PER_IPV4_PREFIX,
            SocketAddr::V6(_) => same_group < MAX_DISCOVERED_PEERS_PER_IPV6_PREFIX,
        }
    }

    fn discovered_peer_count(&self) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.direction == PeerDirection::Discovered)
            .count()
    }

    pub fn promote_discovered_peer(&mut self, address: &str) {
        if let Some(peer) = self.peers.get_mut(address) {
            if peer.direction == PeerDirection::Discovered {
                peer.direction = PeerDirection::Outbound;
            }
        }
    }

    pub fn discovered_peer_count_for_tests(&self) -> usize {
        self.discovered_peer_count()
    }

    pub fn discovered_peer_capacity_for_tests(&self) -> usize {
        MAX_DISCOVERED_PEERS
    }

    pub fn direction_for_tests(&self, address: &str) -> Option<PeerDirection> {
        self.peers.get(address).map(|peer| peer.direction.clone())
    }

    pub fn peer_count_for_tests(&self) -> usize {
        self.peers.len()
    }

    pub fn add_discovered_peer_at(&mut self, address: impl Into<String>, now_ms: u64) -> bool {
        let address = address.into();
        let added = self.add_discovered_peer(address.clone());
        if added {
            if let Some(peer) = self.peers.get_mut(&address) {
                peer.last_contact_ms = Some(now_ms);
            }
        }
        added
    }

    fn prune_stale_discovered_peer(peer: &PeerInfo, now_ms: u64, max_age_ms: u64) -> bool {
        if peer.direction != PeerDirection::Discovered || peer.is_banned_at(now_ms) {
            return true;
        }
        let Some(last_contact) = peer.last_success_ms.or(peer.last_contact_ms) else {
            return false;
        };
        now_ms.saturating_sub(last_contact) <= max_age_ms
    }

    fn prune_stale_inbound_peer(peer: &PeerInfo, now_ms: u64, max_age_ms: u64) -> bool {
        if peer.direction != PeerDirection::Inbound || peer.is_banned_at(now_ms) {
            return true;
        }
        peer.last_contact_ms
            .is_some_and(|last_contact| now_ms.saturating_sub(last_contact) <= max_age_ms)
    }

    pub fn prune_stale_peers_at(
        &mut self,
        now_ms: u64,
        inbound_max_age_ms: u64,
        discovered_max_age_ms: u64,
    ) -> usize {
        let before = self.peers.len();
        self.peers.retain(|_, peer| {
            Self::prune_stale_inbound_peer(peer, now_ms, inbound_max_age_ms)
                && Self::prune_stale_discovered_peer(peer, now_ms, discovered_max_age_ms)
        });
        before.saturating_sub(self.peers.len())
    }

    pub fn observe_inbound_peer(&mut self, address: impl Into<String>) {
        let address = address.into();
        self.peers
            .entry(address.clone())
            .or_insert_with(|| PeerInfo::new(address, PeerDirection::Inbound));
    }

    pub fn replace_peer_address(&mut self, from: &str, to: impl Into<String>) {
        let to = to.into();
        if from == to {
            if !self.peers.contains_key(from) {
                self.add_peer(to);
            }
            return;
        }

        let Some(from_peer) = self.peers.remove(from) else {
            self.add_peer(to);
            return;
        };

        let to_peer = self
            .peers
            .entry(to.clone())
            .or_insert_with(|| PeerInfo::new(to, from_peer.direction.clone()));
        if from_peer.direction == PeerDirection::Outbound {
            to_peer.direction = PeerDirection::Outbound;
        } else if from_peer.direction == PeerDirection::Discovered
            && to_peer.direction == PeerDirection::Inbound
        {
            to_peer.direction = PeerDirection::Discovered;
        }
        to_peer.messages_sent = to_peer
            .messages_sent
            .saturating_add(from_peer.messages_sent);
        to_peer.messages_received = to_peer
            .messages_received
            .saturating_add(from_peer.messages_received);
        to_peer.last_known_height = to_peer.last_known_height.or(from_peer.last_known_height);
        to_peer.last_known_tip_hash = to_peer
            .last_known_tip_hash
            .clone()
            .or(from_peer.last_known_tip_hash);
        if to_peer.last_hello.is_none() {
            to_peer.last_hello = from_peer.last_hello;
        }
        if from_peer.last_clock_observed_ms > to_peer.last_clock_observed_ms {
            to_peer.last_clock_offset_ms = from_peer.last_clock_offset_ms;
            to_peer.last_clock_offset_accepted = from_peer.last_clock_offset_accepted;
            to_peer.last_clock_observed_ms = from_peer.last_clock_observed_ms;
        }
        to_peer.last_contact_ms = to_peer.last_contact_ms.max(from_peer.last_contact_ms);
        to_peer.last_success_ms = to_peer.last_success_ms.max(from_peer.last_success_ms);
        to_peer.last_error_ms = to_peer.last_error_ms.max(from_peer.last_error_ms);
        if to_peer.last_error.is_none() {
            to_peer.last_error = from_peer.last_error;
        }
        to_peer.misbehavior_score = to_peer
            .misbehavior_score
            .saturating_add(from_peer.misbehavior_score);
        to_peer.banned_until_ms = to_peer.banned_until_ms.max(from_peer.banned_until_ms);
        if to_peer.ban_reason.is_none() {
            to_peer.ban_reason = from_peer.ban_reason;
        }
    }

    pub fn remove_peer(&mut self, address: &str) -> bool {
        if self
            .peers
            .get(address)
            .is_some_and(|peer| peer.direction != PeerDirection::Inbound)
        {
            self.peers.remove(address);
            true
        } else {
            false
        }
    }

    pub fn remove_never_connected_peer_if_other_connection_works_at(
        &mut self,
        address: &str,
        now_ms: u64,
    ) -> bool {
        let removable = self.peers.get(address).is_some_and(|peer| {
            peer.direction != PeerDirection::Inbound && peer.last_success_ms.is_none()
        });
        if !removable || !self.has_good_connection_at(now_ms) {
            return false;
        }
        self.peers.remove(address).is_some()
    }

    pub fn is_connectable_peer(&self, address: &str) -> bool {
        self.peers
            .get(address)
            .is_some_and(|peer| peer.direction != PeerDirection::Inbound)
    }

    pub fn addresses(&self) -> Vec<String> {
        self.outbound_addresses_at(now_ms())
    }

    pub fn connectable_addresses_at(&self, now_ms: u64) -> Vec<String> {
        self.peers
            .values()
            .filter(|peer| peer.direction != PeerDirection::Inbound)
            .filter(|peer| !peer.is_banned_at(now_ms))
            .map(|peer| peer.address.clone())
            .collect()
    }

    pub fn outbound_addresses_at(&self, now_ms: u64) -> Vec<String> {
        self.peers
            .values()
            .filter(|peer| peer.direction == PeerDirection::Outbound)
            .filter(|peer| !peer.is_banned_at(now_ms))
            .map(|peer| peer.address.clone())
            .collect()
    }

    pub fn outbound_session_candidates_at(
        &self,
        now_ms: u64,
        max_discovered: usize,
    ) -> Vec<String> {
        let mut outbound = Vec::new();
        let mut discovered = self
            .peers
            .values()
            .filter(|peer| peer.direction == PeerDirection::Discovered)
            .filter(|peer| !peer.is_banned_at(now_ms))
            .cloned()
            .collect::<Vec<_>>();
        discovered.sort_by(|left, right| {
            right
                .last_success_ms
                .cmp(&left.last_success_ms)
                .then_with(|| left.last_error_ms.cmp(&right.last_error_ms))
                .then_with(|| left.address.cmp(&right.address))
        });

        for peer in self
            .peers
            .values()
            .filter(|peer| peer.direction == PeerDirection::Outbound)
            .filter(|peer| !peer.is_banned_at(now_ms))
        {
            outbound.push(peer.address.clone());
        }
        outbound.extend(
            discovered
                .into_iter()
                .take(max_discovered)
                .map(|peer| peer.address),
        );
        outbound
    }

    pub fn addresses_except(&self, excluded: &str) -> Vec<String> {
        self.outbound_addresses_at(now_ms())
            .into_iter()
            .filter(|address| address != excluded)
            .collect()
    }

    pub fn list(&self) -> Vec<PeerInfo> {
        self.peers.values().cloned().collect()
    }

    pub fn prune_stale_inbound_peers_at(&mut self, now_ms: u64, max_age_ms: u64) -> usize {
        self.prune_stale_peers_at(now_ms, max_age_ms, u64::MAX)
    }

    pub fn record_sent(&mut self, address: &str, count: u64) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        if peer.direction == PeerDirection::Discovered {
            peer.direction = PeerDirection::Outbound;
        }
        peer.messages_sent += count;
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_status(&mut self, address: &str, height: u64, tip_hash: String) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        if peer.direction == PeerDirection::Discovered {
            peer.direction = PeerDirection::Outbound;
        }
        peer.last_known_height = Some(height);
        peer.last_known_tip_hash = Some(tip_hash);
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_inbound_status(&mut self, address: &str, height: u64, tip_hash: String) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.messages_received += 1;
        peer.last_known_height = Some(height);
        peer.last_known_tip_hash = Some(tip_hash);
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_hello(&mut self, address: &str, direction: PeerDirection, hello: ProtocolHello) {
        self.ensure(address, direction).last_hello = Some(hello);
    }

    pub fn record_clock_observation(
        &mut self,
        address: &str,
        direction: PeerDirection,
        remote_time_ms: u64,
        local_receive_time_ms: u64,
    ) {
        if remote_time_ms == 0 {
            return;
        }
        let offset = remote_time_ms as i128 - local_receive_time_ms as i128;
        let offset = offset.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        let accepted = offset.abs() <= PEER_CLOCK_OFFSET_ACCEPTANCE_MS;
        let peer = self.ensure(address, direction);
        peer.last_clock_offset_ms = Some(offset);
        peer.last_clock_offset_accepted = Some(accepted);
        peer.last_clock_observed_ms = Some(local_receive_time_ms);
    }

    pub fn network_time_offset_ms_at(&self, now_ms: u64) -> Option<i64> {
        median_i64(
            self.peers
                .values()
                .filter(|peer| !peer.is_banned_at(now_ms))
                .filter(|peer| peer.last_error.is_none())
                .filter(|peer| peer.last_clock_offset_accepted == Some(true))
                .filter(|peer| {
                    peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                        now_ms.saturating_sub(observed_ms) <= PEER_CLOCK_OFFSET_STALE_MS
                    })
                })
                .filter_map(|peer| peer.last_clock_offset_ms)
                .collect(),
        )
    }

    pub fn adjusted_time_ms_at(&self, now_ms: u64) -> u64 {
        match self.network_time_offset_ms_at(now_ms) {
            Some(offset) if offset >= 0 => now_ms.saturating_add(offset as u64),
            Some(offset) => now_ms.saturating_sub(offset.unsigned_abs()),
            None => now_ms,
        }
    }

    pub fn bad_clock_peer_count_at(&self, now_ms: u64) -> usize {
        self.peers
            .values()
            .filter(|peer| !peer.is_banned_at(now_ms))
            .filter(|peer| {
                peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                    now_ms.saturating_sub(observed_ms) <= PEER_CLOCK_OFFSET_STALE_MS
                })
            })
            .filter(|peer| peer.last_clock_offset_accepted == Some(false))
            .count()
    }

    pub fn record_error(&mut self, address: &str, error: impl Into<String>) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Outbound);
        peer.last_contact_ms = Some(now);
        peer.last_error_ms = Some(now);
        peer.last_error = Some(error.into());
    }

    pub fn record_inbound_error(&mut self, address: &str, error: impl Into<String>) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.last_contact_ms = Some(now);
        peer.last_error_ms = Some(now);
        peer.last_error = Some(error.into());
    }

    pub fn record_received(&mut self, address: &str, count: u64) {
        let now = now_ms();
        let peer = self.ensure(address, PeerDirection::Inbound);
        peer.messages_received += count;
        peer.last_contact_ms = Some(now);
        peer.last_success_ms = Some(now);
        if !peer.is_banned_at(now) {
            peer.last_error = None;
            peer.clear_misbehavior();
        }
    }

    pub fn record_misbehavior(&mut self, address: &str, reason: impl Into<String>) {
        self.record_misbehavior_at(address, reason, now_ms());
    }

    pub fn record_misbehavior_at(&mut self, address: &str, reason: impl Into<String>, now_ms: u64) {
        self.record_misbehavior_with_direction(address, reason, now_ms, PeerDirection::Outbound);
    }

    pub fn record_inbound_misbehavior(&mut self, address: &str, reason: impl Into<String>) {
        self.record_misbehavior_with_direction(address, reason, now_ms(), PeerDirection::Inbound);
    }

    fn record_misbehavior_with_direction(
        &mut self,
        address: &str,
        reason: impl Into<String>,
        now_ms: u64,
        direction: PeerDirection,
    ) {
        let reason = reason.into();
        let peer = self.ensure(address, direction);
        peer.last_contact_ms = Some(now_ms);
        peer.last_error_ms = Some(now_ms);
        peer.last_error = Some(reason.clone());
        peer.misbehavior_score = peer.misbehavior_score.saturating_add(1);
        peer.ban_reason = Some(reason);
        if peer.misbehavior_score >= PEER_MISBEHAVIOR_BAN_SCORE {
            peer.banned_until_ms = Some(now_ms.saturating_add(PEER_MISBEHAVIOR_BAN_MS));
        }
    }

    pub fn is_banned(&self, address: &str) -> bool {
        self.is_banned_at(address, now_ms())
    }

    pub fn is_banned_at(&self, address: &str, now_ms: u64) -> bool {
        self.peers
            .get(address)
            .is_some_and(|peer| peer.is_banned_at(now_ms))
    }

    fn ensure(&mut self, address: &str, direction: PeerDirection) -> &mut PeerInfo {
        self.peers
            .entry(address.to_string())
            .or_insert_with(|| PeerInfo::new(address.to_string(), direction))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerInfo {
    pub address: String,
    pub direction: PeerDirection,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub last_known_height: Option<u64>,
    pub last_known_tip_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hello: Option<ProtocolHello>,
    #[serde(default)]
    pub last_clock_offset_ms: Option<i64>,
    #[serde(default)]
    pub last_clock_offset_accepted: Option<bool>,
    #[serde(default)]
    pub last_clock_observed_ms: Option<u64>,
    pub last_error: Option<String>,
    pub last_contact_ms: Option<u64>,
    pub last_success_ms: Option<u64>,
    pub last_error_ms: Option<u64>,
    pub misbehavior_score: u32,
    pub banned_until_ms: Option<u64>,
    pub ban_reason: Option<String>,
}

impl PeerInfo {
    fn new(address: String, direction: PeerDirection) -> Self {
        Self {
            address,
            direction,
            messages_sent: 0,
            messages_received: 0,
            last_known_height: None,
            last_known_tip_hash: None,
            last_hello: None,
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: None,
            last_contact_ms: None,
            last_success_ms: None,
            last_error_ms: None,
            misbehavior_score: 0,
            banned_until_ms: None,
            ban_reason: None,
        }
    }

    pub fn is_banned_at(&self, now_ms: u64) -> bool {
        self.banned_until_ms
            .is_some_and(|banned_until| banned_until > now_ms)
    }

    pub fn is_good_connection_at(&self, now_ms: u64) -> bool {
        !self.is_banned_at(now_ms)
            && self.last_error.is_none()
            && self.last_known_height.is_some()
            && self.last_success_ms.is_some_and(|last_success| {
                now_ms.saturating_sub(last_success) <= super::PEER_GOOD_CONNECTION_MAX_AGE_MS
            })
    }

    pub fn is_good_connection_for_chain_at(
        &self,
        height: u64,
        tip_hash: &str,
        now_ms: u64,
    ) -> bool {
        self.is_good_connection_at(now_ms)
            && self.last_known_height == Some(height)
            && self.last_known_tip_hash.as_deref() == Some(tip_hash)
    }

    fn clear_misbehavior(&mut self) {
        self.misbehavior_score = 0;
        self.banned_until_ms = None;
        self.ban_reason = None;
    }
}

fn median_i64(mut values: Vec<i64>) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn same_discovery_group(left: SocketAddr, right: SocketAddr) -> bool {
    match (left, right) {
        (SocketAddr::V4(left), SocketAddr::V4(right)) => {
            let left = left.ip().octets();
            let right = right.ip().octets();
            left[0] == right[0] && left[1] == right[1]
        }
        (SocketAddr::V6(left), SocketAddr::V6(right)) => {
            let left = left.ip().segments();
            let right = right.ip().segments();
            left[0] == right[0] && left[1] == right[1]
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerDirection {
    Outbound,
    Discovered,
    Inbound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_connection_requires_a_recent_success_without_an_error_or_ban() {
        let now = super::super::PEER_GOOD_CONNECTION_MAX_AGE_MS + 1_000;
        let mut peers = PeerBook::from_addresses(vec!["127.0.0.1:9444".to_string()]);
        assert!(!peers.has_good_connection_at(now));

        peers
            .peers
            .get_mut("127.0.0.1:9444")
            .unwrap()
            .last_success_ms = Some(now - 1_000);
        assert!(!peers.has_good_connection_at(now));

        peers
            .peers
            .get_mut("127.0.0.1:9444")
            .unwrap()
            .last_known_height = Some(1);
        assert!(peers.has_good_connection_at(now));

        peers.peers.get_mut("127.0.0.1:9444").unwrap().last_error = Some("offline".to_string());
        assert!(!peers.has_good_connection_at(now));

        let peer = peers.peers.get_mut("127.0.0.1:9444").unwrap();
        peer.last_error = None;
        peer.banned_until_ms = Some(now + 1);
        assert!(!peers.has_good_connection_at(now));
    }

    #[test]
    fn good_connection_rejects_stale_success() {
        let max_age = super::super::PEER_GOOD_CONNECTION_MAX_AGE_MS;
        let now = max_age + 1_000;
        let mut peers = PeerBook::from_addresses(vec!["127.0.0.1:9444".to_string()]);
        peers
            .peers
            .get_mut("127.0.0.1:9444")
            .unwrap()
            .last_success_ms = Some(now - max_age - 1);
        peers
            .peers
            .get_mut("127.0.0.1:9444")
            .unwrap()
            .last_known_height = Some(1);

        assert!(!peers.has_good_connection_at(now));
    }

    #[test]
    fn good_connection_for_chain_requires_matching_height_and_tip() {
        let now = now_ms();
        let mut peers = PeerBook::from_addresses(vec!["127.0.0.1:9444".to_string()]);
        peers.record_status("127.0.0.1:9444", 42, "peer-tip".to_string());

        assert!(peers.has_good_connection_at(now));
        assert!(!peers.has_good_connection_for_chain_at(41, "peer-tip", now));
        assert!(!peers.has_good_connection_for_chain_at(42, "local-tip", now));
        assert!(peers.has_good_connection_for_chain_at(42, "peer-tip", now));
    }

    #[test]
    fn inbound_status_counts_as_a_good_connection_without_becoming_connectable() {
        let mut peers = PeerBook::default();
        let address = "127.0.0.1:49152";

        peers.record_inbound_status(address, 42, "tip".to_string());

        assert!(peers.has_good_connection_at(now_ms()));
        assert_eq!(peers.list()[0].messages_received, 1);
        assert_eq!(
            peers.direction_for_tests(address),
            Some(PeerDirection::Inbound)
        );
        assert!(!peers.is_connectable_peer(address));
    }

    #[test]
    fn never_connected_peer_is_removed_when_another_connection_works() {
        let now = now_ms();
        let unreachable = "127.0.0.1:9444";
        let working = "127.0.0.1:9445";
        let mut peers =
            PeerBook::from_addresses(vec![unreachable.to_string(), working.to_string()]);
        peers.record_error(unreachable, "connection refused");
        peers.record_status(working, 42, "tip".to_string());

        assert!(peers.remove_never_connected_peer_if_other_connection_works_at(unreachable, now));
        assert!(!peers.is_connectable_peer(unreachable));
        assert!(peers.is_connectable_peer(working));
    }

    #[test]
    fn never_connected_peer_is_kept_when_no_connection_works() {
        let now = now_ms();
        let unreachable = "127.0.0.1:9444";
        let mut peers = PeerBook::from_addresses(vec![unreachable.to_string()]);
        peers.record_error(unreachable, "connection refused");

        assert!(!peers.remove_never_connected_peer_if_other_connection_works_at(unreachable, now));
        assert!(peers.is_connectable_peer(unreachable));
    }

    #[test]
    fn previously_connected_peer_is_never_removed_as_initially_unreachable() {
        let now = now_ms();
        let recovered = "127.0.0.1:9444";
        let working = "127.0.0.1:9445";
        let mut peers = PeerBook::from_addresses(vec![recovered.to_string(), working.to_string()]);
        peers.record_status(recovered, 41, "old-tip".to_string());
        peers.record_error(recovered, "connection refused");
        peers.record_status(working, 42, "tip".to_string());

        assert!(!peers.remove_never_connected_peer_if_other_connection_works_at(recovered, now));
        assert!(peers.is_connectable_peer(recovered));
    }
}
