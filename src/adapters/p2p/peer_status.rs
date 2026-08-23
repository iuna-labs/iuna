use crate::app::now_ms;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PeerStatus {
    pub(super) height: u64,
    pub(super) tip_hash: String,
    pub(super) time_ms: u64,
    pub(super) request_bootstrap: bool,
    pub(super) push_bootstrap: bool,
}

impl PeerStatus {
    pub(super) fn new(height: u64, tip_hash: String) -> Self {
        Self::with_time(height, tip_hash, now_ms())
    }

    pub(super) fn with_time(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            push_bootstrap: false,
        }
    }

    pub(super) fn from_envelope(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            push_bootstrap: false,
        }
    }

    pub(super) fn with_bootstrap_request(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: true,
            push_bootstrap: false,
        }
    }

    pub(super) fn with_bootstrap_push(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            push_bootstrap: true,
        }
    }
}
