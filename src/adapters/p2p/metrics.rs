use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicU64, Ordering},
};

use serde::Serialize;

#[derive(Default)]
pub(super) struct P2pMetricsCounters {
    pub(super) inbound_sessions_started: AtomicU64,
    pub(super) inbound_sessions_rejected: AtomicU64,
    pub(super) outbound_connect_attempts: AtomicU64,
    pub(super) outbound_connect_successes: AtomicU64,
    pub(super) outbound_connect_failures: AtomicU64,
    pub(super) outbound_sessions_started: AtomicU64,
    pub(super) sessions_closed: AtomicU64,
    pub(super) session_failures: AtomicU64,
    pub(super) quiet_disconnects: AtomicU64,
    pub(super) envelopes_received: AtomicU64,
    pub(super) hello_envelopes_received: AtomicU64,
    pub(super) peer_status_envelopes_received: AtomicU64,
    pub(super) inventory_envelopes_received: AtomicU64,
    pub(super) data_envelopes_received: AtomicU64,
    pub(super) blinded_transaction_envelopes_received: AtomicU64,
    pub(super) blinded_transactions_received: AtomicU64,
    pub(super) blinded_reveal_envelopes_received: AtomicU64,
    pub(super) blinded_reveals_received: AtomicU64,
    pub(super) control_envelopes_received: AtomicU64,
    pub(super) bytes_received: AtomicU64,
    pub(super) parse_errors: AtomicU64,
    pub(super) empty_frames: AtomicU64,
    pub(super) self_peer_rejections: AtomicU64,
    pub(super) self_peer_skips: AtomicU64,
    pub(super) outbound_queue_full: AtomicU64,
    pub(super) outbound_queue_closed: AtomicU64,
    pub(super) last_session_failure: StdMutex<Option<String>>,
    pub(super) last_empty_frame_remote: StdMutex<Option<String>>,
    pub(super) last_parse_error: StdMutex<Option<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct P2pMetrics {
    pub inbound_sessions_started: u64,
    pub inbound_sessions_rejected: u64,
    pub outbound_connect_attempts: u64,
    pub outbound_connect_successes: u64,
    pub outbound_connect_failures: u64,
    pub outbound_sessions_started: u64,
    pub sessions_closed: u64,
    pub session_failures: u64,
    pub quiet_disconnects: u64,
    pub envelopes_received: u64,
    pub hello_envelopes_received: u64,
    pub peer_status_envelopes_received: u64,
    pub inventory_envelopes_received: u64,
    pub data_envelopes_received: u64,
    pub blinded_transaction_envelopes_received: u64,
    pub blinded_transactions_received: u64,
    pub blinded_reveal_envelopes_received: u64,
    pub blinded_reveals_received: u64,
    pub control_envelopes_received: u64,
    pub bytes_received: u64,
    pub parse_errors: u64,
    pub empty_frames: u64,
    pub self_peer_rejections: u64,
    pub self_peer_skips: u64,
    pub outbound_queue_full: u64,
    pub outbound_queue_closed: u64,
    pub last_session_failure: Option<String>,
    pub last_empty_frame_remote: Option<String>,
    pub last_parse_error: Option<String>,
}

impl P2pMetricsCounters {
    pub(super) fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn add(counter: &AtomicU64, amount: u64) {
        counter.fetch_add(amount, Ordering::Relaxed);
    }

    pub(super) fn set_last(target: &StdMutex<Option<String>>, value: impl Into<String>) {
        if let Ok(mut last) = target.lock() {
            *last = Some(value.into());
        }
    }

    pub(super) fn snapshot(&self) -> P2pMetrics {
        P2pMetrics {
            inbound_sessions_started: self.inbound_sessions_started.load(Ordering::Relaxed),
            inbound_sessions_rejected: self.inbound_sessions_rejected.load(Ordering::Relaxed),
            outbound_connect_attempts: self.outbound_connect_attempts.load(Ordering::Relaxed),
            outbound_connect_successes: self.outbound_connect_successes.load(Ordering::Relaxed),
            outbound_connect_failures: self.outbound_connect_failures.load(Ordering::Relaxed),
            outbound_sessions_started: self.outbound_sessions_started.load(Ordering::Relaxed),
            sessions_closed: self.sessions_closed.load(Ordering::Relaxed),
            session_failures: self.session_failures.load(Ordering::Relaxed),
            quiet_disconnects: self.quiet_disconnects.load(Ordering::Relaxed),
            envelopes_received: self.envelopes_received.load(Ordering::Relaxed),
            hello_envelopes_received: self.hello_envelopes_received.load(Ordering::Relaxed),
            peer_status_envelopes_received: self
                .peer_status_envelopes_received
                .load(Ordering::Relaxed),
            inventory_envelopes_received: self.inventory_envelopes_received.load(Ordering::Relaxed),
            data_envelopes_received: self.data_envelopes_received.load(Ordering::Relaxed),
            blinded_transaction_envelopes_received: self
                .blinded_transaction_envelopes_received
                .load(Ordering::Relaxed),
            blinded_transactions_received: self
                .blinded_transactions_received
                .load(Ordering::Relaxed),
            blinded_reveal_envelopes_received: self
                .blinded_reveal_envelopes_received
                .load(Ordering::Relaxed),
            blinded_reveals_received: self.blinded_reveals_received.load(Ordering::Relaxed),
            control_envelopes_received: self.control_envelopes_received.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            empty_frames: self.empty_frames.load(Ordering::Relaxed),
            self_peer_rejections: self.self_peer_rejections.load(Ordering::Relaxed),
            self_peer_skips: self.self_peer_skips.load(Ordering::Relaxed),
            outbound_queue_full: self.outbound_queue_full.load(Ordering::Relaxed),
            outbound_queue_closed: self.outbound_queue_closed.load(Ordering::Relaxed),
            last_session_failure: self
                .last_session_failure
                .lock()
                .ok()
                .and_then(|last| last.clone()),
            last_empty_frame_remote: self
                .last_empty_frame_remote
                .lock()
                .ok()
                .and_then(|last| last.clone()),
            last_parse_error: self
                .last_parse_error
                .lock()
                .ok()
                .and_then(|last| last.clone()),
        }
    }
}
