#[cfg(test)]
use crate::app::now_ms;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PeerStatus {
    pub(super) height: u64,
    pub(super) tip_hash: String,
    pub(super) time_ms: u64,
    pub(super) request_bootstrap: bool,
    pub(super) reject_session: bool,
    pub(super) capabilities: Vec<String>,
}

impl PeerStatus {
    #[cfg(test)]
    pub(super) fn new(height: u64, tip_hash: String) -> Self {
        Self::with_time(height, tip_hash, now_ms())
    }

    pub(super) fn with_time(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            reject_session: false,
            capabilities: Vec::new(),
        }
    }

    pub(super) fn from_envelope(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            reject_session: false,
            capabilities: Vec::new(),
        }
    }

    pub(super) fn with_bootstrap_request(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: true,
            reject_session: false,
            capabilities: Vec::new(),
        }
    }

    pub(super) fn rejected(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            request_bootstrap: false,
            reject_session: true,
            capabilities: Vec::new(),
        }
    }

    pub(super) fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub(super) fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .binary_search_by(|candidate| candidate.as_str().cmp(capability))
            .is_ok()
    }
}
