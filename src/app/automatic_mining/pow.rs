use anyhow::{Context, Result};

use super::super::helpers::auto_pow_salt;
use super::super::{
    AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK, AutoPowMineCursor, MINE_ACTIONS_PER_ANCHOR_LIMIT,
    NodeCore, Transaction,
};

impl NodeCore {
    pub fn prepare_automatic_pow_mining(&mut self) -> Result<Option<Transaction>> {
        if self.wallet.is_locked() {
            if self.pow_mining_enabled {
                self.last_auto_pow_mine_status = Some("wallet is locked".to_string());
            }
            return Ok(None);
        }
        if self.pow_mining_enabled && !self.has_real_chain() {
            self.last_auto_pow_mine_status =
                Some("waiting for a real chain before PoW mining can start".to_string());
            self.auto_pow_mine_cursor = None;
            return Ok(None);
        }

        self.prepare_automatic_pow_mine()
    }

    pub fn record_automatic_pow_mining_error(&mut self, message: String) {
        self.last_auto_pow_mine_status = Some(message);
    }

    pub(super) fn prepare_automatic_pow_mine(&mut self) -> Result<Option<Transaction>> {
        if !self.pow_mining_enabled {
            self.last_auto_pow_mine_status = None;
            self.auto_pow_mine_cursor = None;
            return Ok(None);
        }
        let anchor = self
            .ledger
            .chain()
            .last()
            .map(|block| block.hash.clone())
            .context("ledger has no anchor block")?;
        if self.ledger.pending_mine_count_for_anchor(&anchor) >= MINE_ACTIONS_PER_ANCHOR_LIMIT {
            self.last_auto_pow_mine_anchor = Some(anchor);
            self.last_auto_pow_mine_status =
                Some("waiting for next chain tip after queued mine actions".to_string());
            self.auto_pow_mine_cursor = None;
            return Ok(None);
        }
        let wallet_address = self.wallet.address().to_string();
        let needs_cursor = self
            .auto_pow_mine_cursor
            .as_ref()
            .is_none_or(|cursor| cursor.anchor != anchor);
        if needs_cursor {
            self.auto_pow_mine_cursor = Some(AutoPowMineCursor {
                salt: auto_pow_salt(&wallet_address, &anchor),
                anchor: anchor.clone(),
                next_nonce: 0,
                searched: 0,
            });
        }
        let cursor = self
            .auto_pow_mine_cursor
            .as_ref()
            .context("automatic PoW cursor was not initialized")?
            .clone();
        let outcome = self.wallet_build_ledger()?.search_mine(
            wallet_address,
            cursor.salt,
            cursor.next_nonce,
            AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK
                .saturating_mul(u64::from(self.pow_mining_workers)),
        )?;
        let mut searched = outcome.attempts;
        if let Some(cursor) = &mut self.auto_pow_mine_cursor {
            if cursor.anchor == anchor {
                cursor.next_nonce = outcome.next_nonce;
                cursor.searched = cursor.searched.saturating_add(outcome.attempts);
                searched = cursor.searched;
            }
        }
        let Some(tx) = outcome.transaction else {
            self.last_auto_pow_mine_status = Some(format!(
                "searched {searched} PoW nonces for the current tip; no proof yet"
            ));
            return Ok(None);
        };
        self.submit_public_mine_action(tx.clone())?;
        self.last_auto_pow_mine_anchor = Some(anchor);
        self.last_auto_pow_mine_status = Some(format!(
            "queued mine action after {searched} PoW nonce attempts for the current tip"
        ));
        Ok(Some(tx))
    }
}
