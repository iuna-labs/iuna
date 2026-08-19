use std::collections::{BTreeMap, BTreeSet};

use crate::{
    adapters::config_store::{DEFAULT_POW_MINING_WORKERS, clamp_pow_mining_workers},
    domain::{Amount, BurnBundle, DEFAULT_FEE_PER_BYTE, Ledger, Wallet},
};

use super::{GossipEnvelope, NodeConfig, NodeCore, NodeWallet};

impl NodeCore {
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
            last_auto_burn_height: None,
            last_auto_anchor_burn_height: None,
            last_auto_finalization_status: None,
            last_auto_pow_mine_anchor: None,
            last_auto_pow_mine_status: None,
            auto_pow_mine_cursor: None,
            burn_bundles: BTreeMap::<(u64, u8), BurnBundle>::new(),
            equivocated_burn_bundle_slots: BTreeSet::new(),
            burn_bundle_collection_started: None,
            local_block_anchor_burn: None,
            outbox: Vec::<GossipEnvelope>::new(),
        }
    }

    pub fn wallet_address(&self) -> &str {
        self.wallet.address()
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

    pub fn reset_chain_to_setup_placeholder(&mut self) {
        self.ledger = Ledger::new(BTreeMap::new(), 1);
        self.reset_automatic_mining_progress();
        self.burn_bundles.clear();
        self.equivocated_burn_bundle_slots.clear();
        self.burn_bundle_collection_started = None;
        self.local_block_anchor_burn = None;
        self.outbox.clear();
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
