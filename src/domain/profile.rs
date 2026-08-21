use serde::{Deserialize, Serialize};

use super::{
    BURN_LINEAGE_MATURITY_HEIGHTS, DEFAULT_TICKET_EXPIRY_WINDOW, DEFAULT_TICKET_MATURITY_DELAY,
    MAX_BLOCK_BYTES, MAX_BLOCK_TRANSACTIONS, MAX_PENDING_TRANSACTIONS, MINE_DIFFICULTY_BITS,
    hex_hash,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfile {
    pub profile_id: String,
    pub ticket_maturity_delay_heights: u64,
    #[serde(default = "default_ticket_expiry_window_heights")]
    pub ticket_expiry_window_heights: u64,
    #[serde(default = "default_mine_difficulty_bits")]
    pub mine_difficulty_bits: u32,
    #[serde(default = "default_burn_lineage_maturity_heights")]
    pub burn_lineage_maturity_heights: u64,
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
            burn_lineage_maturity_heights: BURN_LINEAGE_MATURITY_HEIGHTS,
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

fn default_burn_lineage_maturity_heights() -> u64 {
    BURN_LINEAGE_MATURITY_HEIGHTS
}

impl LaunchProfile {
    pub fn local_testnet() -> Self {
        Self {
            profile_id: "iuna-local-testnet-v1".to_string(),
            burn_lineage_maturity_heights: 0,
            ..Self::default()
        }
    }

    pub fn hash(&self) -> String {
        let canonical = if self.burn_lineage_maturity_heights == BURN_LINEAGE_MATURITY_HEIGHTS {
            // Preserve the existing mainnet-candidate profile hash. The maturity
            // used to be a frozen protocol constant, so including its unchanged
            // default would needlessly split the existing network.
            format!(
                "iuna-launch-profile:{}:{}:{}:{}:{}:{}:{}",
                self.profile_id,
                self.ticket_maturity_delay_heights,
                self.ticket_expiry_window_heights,
                self.mine_difficulty_bits,
                self.max_pending_transactions,
                self.max_block_transactions,
                self.max_block_bytes
            )
        } else {
            format!(
                "iuna-launch-profile-v2:{}:{}:{}:{}:{}:{}:{}:{}",
                self.profile_id,
                self.ticket_maturity_delay_heights,
                self.ticket_expiry_window_heights,
                self.mine_difficulty_bits,
                self.burn_lineage_maturity_heights,
                self.max_pending_transactions,
                self.max_block_transactions,
                self.max_block_bytes
            )
        };
        hex_hash(canonical)
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
        assert_eq!(
            profile.burn_lineage_maturity_heights,
            crate::domain::BURN_LINEAGE_MATURITY_HEIGHTS
        );
        assert_eq!(profile.max_pending_transactions, MAX_PENDING_TRANSACTIONS);
        assert_eq!(profile.max_block_transactions, MAX_BLOCK_TRANSACTIONS);
        assert_eq!(profile.max_block_bytes, MAX_BLOCK_BYTES);
        assert_eq!(
            profile.hash(),
            "aef51531eaa3a5c5d3ea8a2524ffba029dcb106e4b0a432b57d5ac1f4f8963de"
        );
    }

    #[test]
    fn local_testnet_profile_has_immediate_burn_lineage_eligibility() {
        let profile = LaunchProfile::local_testnet();

        assert_eq!(profile.profile_id, "iuna-local-testnet-v1");
        assert_eq!(profile.burn_lineage_maturity_heights, 0);
        assert_ne!(profile.hash(), LaunchProfile::default().hash());
    }

    #[test]
    fn legacy_json_profile_defaults_burn_lineage_maturity() {
        let profile: LaunchProfile = serde_json::from_str(
            r#"{
                "profile_id":"legacy",
                "ticket_maturity_delay_heights":3,
                "ticket_expiry_window_heights":3,
                "mine_difficulty_bits":12,
                "max_pending_transactions":10000,
                "max_block_transactions":1000,
                "max_block_bytes":1000000
            }"#,
        )
        .unwrap();

        assert_eq!(
            profile.burn_lineage_maturity_heights,
            crate::domain::BURN_LINEAGE_MATURITY_HEIGHTS
        );
    }

    #[test]
    fn genesis_burn_constructor_preserves_fields() {
        let burn = GenesisBurn::new("alice", 42);

        assert_eq!(burn.from, "alice");
        assert_eq!(burn.amount, 42);
    }
}
