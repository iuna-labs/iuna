use serde::{Deserialize, Serialize};

use super::{
    DEFAULT_TICKET_EXPIRY_WINDOW, DEFAULT_TICKET_MATURITY_DELAY, MAX_BLOCK_BYTES,
    MAX_BLOCK_TRANSACTIONS, MAX_PENDING_TRANSACTIONS, MINE_DIFFICULTY_BITS, hex_hash,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfile {
    pub profile_id: String,
    pub ticket_maturity_delay_heights: u64,
    #[serde(default = "default_ticket_expiry_window_heights")]
    pub ticket_expiry_window_heights: u64,
    #[serde(default = "default_mine_difficulty_bits")]
    pub mine_difficulty_bits: u32,
    pub max_pending_transactions: usize,
    pub max_block_transactions: usize,
    #[serde(default = "default_max_block_bytes")]
    pub max_block_bytes: usize,
}

impl Default for LaunchProfile {
    fn default() -> Self {
        Self {
            profile_id: "iuna-mainnet-candidate-v1".to_string(),
            ticket_maturity_delay_heights: DEFAULT_TICKET_MATURITY_DELAY,
            ticket_expiry_window_heights: DEFAULT_TICKET_EXPIRY_WINDOW,
            mine_difficulty_bits: MINE_DIFFICULTY_BITS,
            max_pending_transactions: MAX_PENDING_TRANSACTIONS,
            max_block_transactions: MAX_BLOCK_TRANSACTIONS,
            max_block_bytes: MAX_BLOCK_BYTES,
        }
    }
}

fn default_max_block_bytes() -> usize {
    MAX_BLOCK_BYTES
}

fn default_ticket_expiry_window_heights() -> u64 {
    DEFAULT_TICKET_EXPIRY_WINDOW
}

fn default_mine_difficulty_bits() -> u32 {
    MINE_DIFFICULTY_BITS
}

impl LaunchProfile {
    pub fn hash(&self) -> String {
        hex_hash(format!(
            "iuna-launch-profile:{}:{}:{}:{}:{}:{}:{}",
            self.profile_id,
            self.ticket_maturity_delay_heights,
            self.ticket_expiry_window_heights,
            self.mine_difficulty_bits,
            self.max_pending_transactions,
            self.max_block_transactions,
            self.max_block_bytes
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenesisBurn {
    pub from: String,
    pub amount: u64,
}

impl GenesisBurn {
    pub fn new(from: impl Into<String>, amount: u64) -> Self {
        Self {
            from: from.into(),
            amount,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GenesisBurn, LaunchProfile};
    use crate::domain::{
        DEFAULT_TICKET_EXPIRY_WINDOW, DEFAULT_TICKET_MATURITY_DELAY, MAX_BLOCK_BYTES,
        MAX_BLOCK_TRANSACTIONS, MAX_PENDING_TRANSACTIONS, MINE_DIFFICULTY_BITS,
    };

    #[test]
    fn default_launch_profile_matches_protocol_defaults() {
        let profile = LaunchProfile::default();

        assert_eq!(profile.profile_id, "iuna-mainnet-candidate-v1");
        assert_eq!(
            profile.ticket_maturity_delay_heights,
            DEFAULT_TICKET_MATURITY_DELAY
        );
        assert_eq!(
            profile.ticket_expiry_window_heights,
            DEFAULT_TICKET_EXPIRY_WINDOW
        );
        assert_eq!(profile.mine_difficulty_bits, MINE_DIFFICULTY_BITS);
        assert_eq!(profile.max_pending_transactions, MAX_PENDING_TRANSACTIONS);
        assert_eq!(profile.max_block_transactions, MAX_BLOCK_TRANSACTIONS);
        assert_eq!(profile.max_block_bytes, MAX_BLOCK_BYTES);
        assert_eq!(
            profile.hash(),
            "aef51531eaa3a5c5d3ea8a2524ffba029dcb106e4b0a432b57d5ac1f4f8963de"
        );
    }

    #[test]
    fn genesis_burn_constructor_preserves_fields() {
        let burn = GenesisBurn::new("alice", 42);

        assert_eq!(burn.from, "alice");
        assert_eq!(burn.amount, 42);
    }
}
