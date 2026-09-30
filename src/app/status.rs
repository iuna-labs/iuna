use anyhow::Result;

use crate::{
    adapters::config_store::{MAX_POW_MINING_WORKERS, clamp_pow_mining_workers},
    domain::{
        AddressNetwork, Amount, MINE_FINALIZER_FEE, Transaction, TransactionV2,
        VDF_TARGET_BLOCK_MS, transaction_v2_is_active,
    },
};

use super::{
    FundedWalletAddressStatus, LaunchProfileStatus, MiningStatus, NETWORK_ID,
    NetworkMigrationStatus, NodeCore, NodeStatus, QuantumMigrationStatus, StratumStatus,
    helpers::{transaction_input_total_from_outputs, transaction_output_total_for_address},
    now_ms,
};

impl NodeCore {
    pub fn status(&self) -> NodeStatus {
        let chain = self.ledger.light_status();
        let launch_profile = self.ledger.launch_profile();
        let current_leader = self.ledger.expected_leader_for_next_block();
        let wallet_is_current_leader = current_leader
            .as_deref()
            .is_none_or(|leader| leader == self.wallet.address());
        let legacy_address = self.wallet.address();
        let legacy_balance = self.ledger.balance_of(legacy_address);
        let legacy_utxos = self.ledger.utxos_for_address(legacy_address).len();
        let hybrid_address = self.wallet.unlocked().ok().map(|wallet| {
            wallet.hybrid_address(AddressNetwork::from_profile_id(
                &self.ledger.launch_profile().profile_id,
            ))
        });
        let hybrid_addresses = self
            .wallet
            .unlocked()
            .ok()
            .and_then(|wallet| {
                self.ledger
                    .wallet_owned_hybrid_encoded_addresses(wallet)
                    .ok()
            })
            .unwrap_or_default();
        let hybrid_balance = hybrid_addresses.iter().fold(0_u64, |total, address| {
            total.saturating_add(self.ledger.balance_of(address))
        });
        let pending_spent = self.wallet_pending_spent_outpoints();
        let mut owned_addresses = vec![legacy_address.to_string()];
        owned_addresses.extend(hybrid_addresses);
        owned_addresses.sort();
        owned_addresses.dedup();
        let wallet_receive_address = self.wallet_receive_address().unwrap_or_default();
        if !wallet_receive_address.is_empty() {
            owned_addresses.push(wallet_receive_address.clone());
            owned_addresses.sort();
            owned_addresses.dedup();
        }
        let mut funded_wallet_addresses = owned_addresses
            .iter()
            .cloned()
            .filter_map(|address| {
                let utxos = self.ledger.utxos_for_address(&address);
                if utxos.is_empty() {
                    return None;
                }
                Some(FundedWalletAddressStatus {
                    legacy: address == legacy_address,
                    address,
                    balance: utxos.iter().fold(0_u64, |total, (_, output)| {
                        total.saturating_add(output.amount)
                    }),
                    utxos: utxos.len(),
                    spendable_utxos: utxos
                        .iter()
                        .filter(|(outpoint, _)| !pending_spent.contains(outpoint))
                        .count(),
                })
            })
            .collect::<Vec<_>>();
        funded_wallet_addresses.sort_by(|left, right| {
            right
                .balance
                .cmp(&left.balance)
                .then_with(|| left.address.cmp(&right.address))
        });
        let address_network =
            AddressNetwork::from_profile_id(&self.ledger.launch_profile().profile_id);
        let transaction_v2_domain = self.ledger.transaction_v2_domain().ok();
        let pending_transaction_id = hybrid_address.as_deref().and_then(|address| {
            self.ledger.pending_v2().iter().find_map(|transaction| {
                let TransactionV2::Migration { outputs, .. } = transaction else {
                    return None;
                };
                let belongs_to_wallet = outputs.iter().any(|output| {
                    crate::domain::encode_versioned_address(output.address, address_network)
                        .is_ok_and(|output_address| output_address == address)
                });
                belongs_to_wallet.then(|| {
                    transaction_v2_domain
                        .as_ref()
                        .and_then(|domain| transaction.transaction_id(domain).ok())
                        .map(crate::domain::hex_encode)
                })?
            })
        });

        NodeStatus {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            wallet_address: self.wallet.address().to_string(),
            wallet_receive_address,
            wallet_owned_addresses: owned_addresses,
            funded_wallet_addresses,
            wallet_balance: self.wallet_projected_balance(),
            wallet_locked: self.wallet.is_locked(),
            quantum_migration: QuantumMigrationStatus {
                active: transaction_v2_is_active(self.ledger.height()),
                hybrid_address,
                legacy_balance,
                hybrid_balance,
                legacy_utxos,
                migration_pending: pending_transaction_id.is_some(),
                pending_transaction_id,
            },
            launch_profile: LaunchProfileStatus {
                profile_id: launch_profile.profile_id.clone(),
                profile_hash: chain.launch_profile_hash.clone(),
                ticket_maturity_delay_heights: launch_profile.ticket_maturity_delay_heights,
                ticket_expiry_window_heights: launch_profile.ticket_expiry_window_heights,
                mine_difficulty_bits: launch_profile.mine_difficulty_bits,
                burn_lineage_maturity_heights: launch_profile.burn_lineage_maturity_heights,
            },
            mining: MiningStatus {
                automatic: self.automatic_mining_enabled,
                pow_mining_enabled: self.pow_mining_enabled,
                pow_mining_workers: self.pow_mining_workers,
                max_pow_mining_workers: MAX_POW_MINING_WORKERS,
                burn_per_block: self.burn_per_block,
                automatic_burn_fee: self.burn_fee,
                automatic_pow_mine_fee: MINE_FINALIZER_FEE,
                last_auto_finalization_status: self.last_auto_finalization_status.clone(),
                last_auto_pow_mine_anchor: self.last_auto_pow_mine_anchor.clone(),
                last_auto_pow_mine_status: if self.pow_mining_enabled && !self.has_real_chain() {
                    Some("waiting for a real chain before PoW mining can start".to_string())
                } else {
                    self.last_auto_pow_mine_status.clone()
                },
                vdf_rounds: self.ledger.vdf_rounds(),
                vdf_target_block_ms: VDF_TARGET_BLOCK_MS,
                current_leader,
                wallet_is_current_leader,
                last_auto_burn_height: self.last_auto_burn_height,
                recovery_vdf_top_rank_percent: self.recovery_vdf_top_rank_percent,
            },
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            chain,
            network_migration: NetworkMigrationStatus {
                required: self.network_migration_from().is_some(),
                from_network: self.network_migration_from().map(str::to_string),
                to_network: NETWORK_ID.to_string(),
            },
            ui_data_ready: true,
        }
    }

    fn wallet_projected_balance(&self) -> Amount {
        let address = self.wallet.address();
        let mut balance = self.ledger.balance_of(address);
        if let Ok(wallet) = self.wallet.unlocked() {
            if let Ok(addresses) = self.ledger.wallet_owned_hybrid_encoded_addresses(wallet) {
                for hybrid_address in addresses {
                    balance = balance.saturating_add(self.ledger.balance_of(&hybrid_address));
                }
            }
        }
        let confirmed_outputs = self
            .ledger
            .utxos_for_address(address)
            .into_iter()
            .map(|(outpoint, output)| (outpoint, output.amount))
            .collect::<std::collections::BTreeMap<_, _>>();
        if let Some((height, burn)) = &self.local_block_anchor_burn {
            if *height == self.ledger.height() && !self.ledger.has_transaction(burn.signature()) {
                let output_total = transaction_output_total_for_address(burn, address);
                let input_total =
                    transaction_input_total_from_outputs(burn, address, &confirmed_outputs);
                balance = balance
                    .saturating_sub(input_total)
                    .saturating_add(output_total);
            }
        }

        balance
    }

    pub fn set_burn_per_block(&mut self, amount: Amount) -> Result<Option<Transaction>> {
        self.set_automatic_burn(amount, self.burn_fee)
    }

    pub fn set_automatic_burn(
        &mut self,
        amount: Amount,
        fee: Amount,
    ) -> Result<Option<Transaction>> {
        self.set_automatic_burn_settings(amount > 0, amount, fee)
    }

    pub fn set_automatic_burn_settings(
        &mut self,
        enabled: bool,
        amount: Amount,
        fee: Amount,
    ) -> Result<Option<Transaction>> {
        let was_disabled = !self.automatic_mining_enabled || self.burn_per_block == 0;
        self.automatic_mining_enabled = enabled;
        self.burn_per_block = amount;
        self.burn_fee = fee;
        if was_disabled && enabled && amount > 0 {
            self.last_auto_burn_height = None;
            self.last_auto_anchor_burn_height = None;
        }
        self.prepare_automatic_burn(now_ms())
    }

    pub fn automatic_mining_enabled(&self) -> bool {
        self.automatic_mining_enabled
    }

    pub fn set_pow_mining_enabled(&mut self, enabled: bool) {
        self.pow_mining_enabled = enabled;
        self.auto_pow_mine_cursor = None;
        if !enabled {
            self.last_auto_pow_mine_anchor = None;
            self.last_auto_pow_mine_status = None;
        } else {
            self.last_auto_pow_mine_status =
                Some("waiting for next automatic PoW mining tick".to_string());
        }
    }

    pub fn pow_mining_enabled(&self) -> bool {
        self.pow_mining_enabled
    }

    pub fn set_pow_mining_workers(&mut self, workers: u8) {
        let workers = clamp_pow_mining_workers(workers);
        if self.pow_mining_workers != workers {
            self.pow_mining_workers = workers;
            self.auto_pow_mine_cursor = None;
            if self.pow_mining_enabled {
                self.last_auto_pow_mine_status =
                    Some("waiting for next automatic PoW mining tick".to_string());
            }
        }
    }

    pub fn pow_mining_workers(&self) -> u8 {
        self.pow_mining_workers
    }

    pub fn record_automatic_finalization_status(&mut self, message: impl Into<String>) {
        self.last_auto_finalization_status = Some(message.into());
    }

    pub fn set_recovery_vdf_top_rank_percent(&mut self, percent: u8) {
        self.recovery_vdf_top_rank_percent = percent.min(100);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{NodeConfig, NodeCore},
        domain::{LaunchProfile, Ledger, Wallet},
    };

    #[test]
    fn status_reports_package_version() {
        let wallet = Wallet::from_seed("status-version-wallet");
        let node = NodeCore::new(NodeConfig {
            wallet,
            genesis_allocations: BTreeMap::new(),
            vdf_rounds: 1,
            burn_per_block: 0,
            burn_fee: 0,
            pow_mining_workers: 1,
            recovery_vdf_top_rank_percent: 100,
        });

        let status = node.status();
        assert_eq!(status.app_version, env!("CARGO_PKG_VERSION"));
        assert!(status.wallet_receive_address.starts_with("iuna1q"));
        assert!(
            status
                .wallet_owned_addresses
                .contains(&status.wallet_address)
        );
        assert!(
            status
                .wallet_owned_addresses
                .contains(&status.wallet_receive_address)
        );
    }

    #[test]
    fn local_testnet_status_uses_a_distinct_receive_address_prefix() {
        let wallet = Wallet::from_seed("status-testnet-wallet");
        let ledger = Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::new(),
            Vec::new(),
            1,
            LaunchProfile::local_testnet(),
        )
        .unwrap();
        let node = NodeCore::from_ledger(wallet, ledger, 0);
        let status = node.status();

        assert!(status.wallet_receive_address.starts_with("tiuna1q"));
        assert!(
            node.normalize_user_address(&status.wallet_receive_address)
                .is_ok()
        );
        let wrong_network = status.wallet_receive_address.replacen("tiuna", "iuna", 1);
        assert!(node.normalize_user_address(&wrong_network).is_err());

        let receive_address = status.wallet_receive_address;
        let mut reset_node = node;
        reset_node.reset_chain_to_setup_placeholder();
        assert_eq!(
            reset_node.wallet_receive_address().unwrap(),
            receive_address
        );
    }

    #[test]
    fn status_does_not_panic_on_invalid_locked_wallet_metadata() {
        let node = NodeCore::from_locked_wallet_address(
            "corrupt-wallet-address",
            Ledger::new(BTreeMap::new(), 1),
            false,
            0,
            0,
        );

        let status = node.status();

        assert_eq!(status.wallet_address, "corrupt-wallet-address");
        assert!(status.wallet_receive_address.is_empty());
        assert_eq!(
            status.wallet_owned_addresses,
            vec!["corrupt-wallet-address"]
        );
    }

    #[test]
    fn status_omits_full_balance_map_for_ui_polling() {
        let wallet = Wallet::from_seed("status-light-wallet");
        let wallet_address = wallet.address().to_string();
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet_address.clone(), 10);
        allocations.insert(
            Wallet::from_seed("status-light-peer").address().to_string(),
            5,
        );
        let ledger = Ledger::new(allocations, 1);
        let node = NodeCore::from_ledger(wallet, ledger, 0);

        let status = node.status();

        assert_eq!(status.wallet_balance, 10);
        assert_eq!(status.funded_wallet_addresses.len(), 1);
        assert_eq!(status.funded_wallet_addresses[0].address, wallet_address);
        assert_eq!(status.funded_wallet_addresses[0].balance, 10);
        assert_eq!(status.funded_wallet_addresses[0].utxos, 1);
        assert_eq!(status.funded_wallet_addresses[0].spendable_utxos, 1);
        assert!(status.funded_wallet_addresses[0].legacy);
        assert!(status.chain.balances.is_empty());
    }

    #[test]
    fn automatic_pow_status_reports_setup_placeholder_wait() {
        let wallet = Wallet::from_seed("automatic-pow-setup-placeholder-wallet");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);

        node.set_pow_mining_enabled(true);

        assert_eq!(
            node.status().mining.last_auto_pow_mine_status.as_deref(),
            Some("waiting for a real chain before PoW mining can start")
        );
    }

    #[test]
    fn status_exposes_required_network_migration() {
        let wallet = Wallet::from_seed("network-migration-status-wallet");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);

        node.require_network_migration("iuna-devnet-v5");
        let status = node.status();

        assert!(status.network_migration.required);
        assert_eq!(
            status.network_migration.from_network.as_deref(),
            Some("iuna-devnet-v5")
        );
        assert_eq!(status.network_migration.to_network, crate::app::NETWORK_ID);
    }
}
