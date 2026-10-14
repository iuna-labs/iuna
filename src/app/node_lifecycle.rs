use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use crate::{
    adapters::config_store::{DEFAULT_POW_MINING_WORKERS, clamp_pow_mining_workers},
    domain::{
        AddressNetwork, Amount, BurnBundle, DEFAULT_FEE_PER_BYTE, Ledger, VersionedAddress, Wallet,
        decode_address, decode_versioned_address, encode_address, transaction_v2_is_active,
    },
};

use super::{NodeConfig, NodeCore, NodeWallet};

impl NodeCore {
    /// Keep only a recent window of committed history resident from now on.
    pub fn bound_resident_chain_history(&mut self) {
        self.bounded_history = true;
        self.prune_persisted_history();
    }

    /// Stop pruning, for modes such as metrics tracking that need the complete chain resident.
    pub fn keep_complete_chain_history(&mut self) {
        self.bounded_history = false;
    }

    pub fn bounds_resident_chain_history(&self) -> bool {
        self.bounded_history
    }

    /// Record that the chain store has committed the chain up to and including this block.
    pub fn mark_chain_persisted(&mut self, height: u64, hash: &str) {
        self.persisted_tip = Some((height, hash.to_string()));
        self.prune_persisted_history();
    }

    /// Reattach pruned history loaded from the chain store, provided it still matches the tip.
    pub fn restore_complete_chain_history(&mut self, complete: Ledger) -> Result<bool> {
        self.ledger.restore_pruned_history(complete)
    }

    /// Prune resident history down to the recent window, but never past the persisted tip.
    pub(super) fn prune_persisted_history(&mut self) {
        if !self.bounded_history {
            return;
        }
        let Some((persisted_height, persisted_hash)) = self.persisted_tip.as_ref() else {
            return;
        };
        // After a reorg below the persisted tip the store must first be rewritten from the
        // fork point, which needs the blocks above it to stay resident.
        if self
            .ledger
            .resident_block_at_height(*persisted_height)
            .is_none_or(|block| block.hash != *persisted_hash)
        {
            return;
        }
        let unpersisted = self.ledger.height().saturating_sub(*persisted_height);
        let keep = usize::try_from(unpersisted)
            .unwrap_or(usize::MAX)
            .max(super::RESIDENT_CHAIN_BLOCKS);
        self.ledger.retain_recent_chain_blocks(keep);
    }

    pub fn new(config: NodeConfig) -> Self {
        let ledger = Ledger::new(config.genesis_allocations, config.vdf_rounds);
        let mut node = Self::from_ledger_with_burn_fee(
            config.wallet,
            ledger,
            config.burn_per_block,
            config.burn_fee,
        );
        node.set_pow_mining_workers(config.pow_mining_workers);
        node.set_recovery_vdf_top_rank_percent(config.recovery_vdf_top_rank_percent);
        node
    }

    pub fn from_ledger(wallet: Wallet, ledger: Ledger, burn_per_block: Amount) -> Self {
        Self::from_ledger_with_burn_fee(wallet, ledger, burn_per_block, DEFAULT_FEE_PER_BYTE)
    }

    pub fn from_locked_wallet_address(
        address: impl Into<String>,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_node_wallet_with_burn_fee_and_enabled(
            NodeWallet::Locked {
                address: address.into(),
            },
            ledger,
            automatic_mining_enabled,
            burn_per_block,
            burn_fee,
            DEFAULT_POW_MINING_WORKERS,
            100,
        )
    }

    pub fn from_ledger_with_burn_fee(
        wallet: Wallet,
        ledger: Ledger,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_ledger_with_burn_fee_and_enabled(
            wallet,
            ledger,
            burn_per_block > 0,
            burn_per_block,
            burn_fee,
        )
    }

    pub fn from_ledger_with_burn_fee_and_enabled(
        wallet: Wallet,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
    ) -> Self {
        Self::from_node_wallet_with_burn_fee_and_enabled(
            NodeWallet::Unlocked(wallet),
            ledger,
            automatic_mining_enabled,
            burn_per_block,
            burn_fee,
            DEFAULT_POW_MINING_WORKERS,
            100,
        )
    }

    fn from_node_wallet_with_burn_fee_and_enabled(
        wallet: NodeWallet,
        ledger: Ledger,
        automatic_mining_enabled: bool,
        burn_per_block: Amount,
        burn_fee: Amount,
        pow_mining_workers: u8,
        recovery_vdf_top_rank_percent: u8,
    ) -> Self {
        Self {
            wallet,
            ledger,
            automatic_mining_enabled,
            pow_mining_enabled: false,
            pow_mining_workers: clamp_pow_mining_workers(pow_mining_workers),
            burn_per_block,
            burn_fee,
            recovery_vdf_top_rank_percent: recovery_vdf_top_rank_percent.min(100),
            vdf_memory_mib: crate::domain::DEFAULT_VDF_MEMORY_MIB,
            last_auto_burn_height: None,
            last_auto_anchor_burn_height: None,
            last_auto_finalization_status: None,
            last_auto_pow_mine_anchor: None,
            last_auto_pow_mine_status: None,
            auto_pow_mine_cursor: None,
            burn_bundles: BTreeMap::<(u64, u8, String), BurnBundle>::new(),
            equivocated_burn_bundle_slots: BTreeSet::new(),
            burn_bundle_collection_started: None,
            local_block_anchor_burn: None,
            outbox: std::collections::VecDeque::new(),
            outbox_bytes: 0,
            network_migration_from: None,
            vdf_speed_sample: None,
            bounded_history: false,
            persisted_tip: None,
        }
    }

    pub fn require_network_migration(&mut self, from_network: impl Into<String>) {
        self.network_migration_from = Some(from_network.into());
    }

    pub fn network_migration_from(&self) -> Option<&str> {
        self.network_migration_from.as_deref()
    }

    pub fn complete_network_migration(&mut self) {
        self.network_migration_from = None;
    }

    pub fn wallet_address(&self) -> &str {
        self.wallet.address()
    }

    pub fn wallet_receive_address(&self) -> Result<String> {
        if transaction_v2_is_active(self.ledger.height()) {
            if let Ok(wallet) = self.wallet.unlocked() {
                return self.ledger.wallet_receive_address(wallet);
            }
        }
        encode_address(self.wallet.address(), self.address_network())
    }

    pub fn wallet_owned_addresses(&self) -> Result<Vec<String>> {
        let mut addresses = vec![self.wallet.address().to_string()];
        if let Ok(wallet) = self.wallet.unlocked() {
            addresses.extend(self.ledger.wallet_owned_hybrid_encoded_addresses(wallet)?);
        }
        addresses.sort();
        addresses.dedup();
        Ok(addresses)
    }

    pub fn normalize_user_address(&self, address: &str) -> Result<String> {
        decode_address(address, self.address_network())
    }

    pub fn decode_user_address(&self, address: &str) -> Result<VersionedAddress> {
        decode_versioned_address(address, self.address_network())
    }

    fn address_network(&self) -> AddressNetwork {
        AddressNetwork::from_profile_id(&self.ledger.launch_profile().profile_id)
    }

    pub fn wallet_is_locked(&self) -> bool {
        self.wallet.is_locked()
    }

    pub fn replace_wallet(&mut self, wallet: Wallet) {
        self.wallet = NodeWallet::Unlocked(wallet);
        self.reset_automatic_mining_progress();
        self.burn_bundles.clear();
        self.equivocated_burn_bundle_slots.clear();
        self.burn_bundle_collection_started = None;
        self.local_block_anchor_burn = None;
    }

    pub fn reset_chain_to_setup_placeholder(&mut self) -> Result<()> {
        let launch_profile = self.ledger.launch_profile().clone();
        self.ledger = Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::new(),
            Vec::new(),
            1,
            launch_profile,
        )
        .context("failed to construct setup ledger")?;
        self.persisted_tip = None;
        self.reset_automatic_mining_progress();
        self.burn_bundles.clear();
        self.equivocated_burn_bundle_slots.clear();
        self.burn_bundle_collection_started = None;
        self.local_block_anchor_burn = None;
        self.outbox.clear();
        Ok(())
    }

    pub(super) fn reset_automatic_mining_progress(&mut self) {
        self.last_auto_burn_height = None;
        self.last_auto_anchor_burn_height = None;
        self.last_auto_finalization_status = None;
        self.last_auto_pow_mine_anchor = None;
        self.last_auto_pow_mine_status = None;
        self.auto_pow_mine_cursor = None;
        self.burn_bundle_collection_started = None;
    }
}
