use super::hex::{decode_hex, decode_hex_array, hex_encode};
use super::ledger_ops::{compact_block_context, ensure_transaction_v2_fits_empty_block};
use super::mining::mine_signature;
use super::stratum::{
    hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature, stratum_mine_template,
};
use super::transaction::{UnsignedTxInput, UnsignedUtxoTransaction};
use super::validation::validate_address;
use super::{
    AddressNetwork, Amount, HYBRID_REWARD_ACTIVATION_HEIGHT, HybridAddressBranch, Ledger,
    LegacyTransactionId, MineSearchOutcome, OutPoint, StratumMineShare, StratumMineTemplate,
    Transaction, TransactionV2, TransactionV2Domain, TransactionV2Input, TransactionV2LegacyInput,
    TransactionV2Output, TxOutput, VersionedAddress, Wallet, decode_versioned_address,
    encode_versioned_address,
};
use anyhow::{Context, Result, bail};

pub const HYBRID_EXTERNAL_ADDRESS_GAP_LIMIT: u32 = 20;
const MAX_DISCOVERED_EXTERNAL_ADDRESSES: u32 = 10_000;

impl Ledger {
    pub fn wallet_receive_address(&self, wallet: &Wallet) -> Result<String> {
        let (_, address) = self
            .wallet_external_addresses(wallet)?
            .last()
            .copied()
            .context(
                "wallet external address discovery did not return a current receive address",
            )?;
        encode_versioned_address(address, self.address_network())
    }

    pub fn wallet_reward_address(&self, wallet: &Wallet, _height: u64) -> String {
        let (_, address) = self
            .wallet_reward_addresses(wallet)
            .expect("valid chain has discoverable wallet reward addresses")
            .last()
            .copied()
            .expect("wallet reward discovery always returns a current address");
        encode_versioned_address(address, self.address_network())
            .expect("wallet reward address has a valid fixed-size commitment")
    }

    pub fn wallet_owned_hybrid_addresses(&self, wallet: &Wallet) -> Result<Vec<VersionedAddress>> {
        let mut addresses = self
            .wallet_external_addresses(wallet)?
            .into_iter()
            .map(|(_, address)| address)
            .collect::<Vec<_>>();
        if self.height() >= HYBRID_REWARD_ACTIVATION_HEIGHT {
            for (_, address) in self.wallet_reward_addresses(wallet)? {
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
        }
        Ok(addresses.into_iter().collect())
    }

    pub fn wallet_owned_hybrid_encoded_addresses(&self, wallet: &Wallet) -> Result<Vec<String>> {
        self.wallet_owned_hybrid_addresses(wallet)?
            .into_iter()
            .map(|address| encode_versioned_address(address, self.address_network()))
            .collect()
    }

    fn address_network(&self) -> AddressNetwork {
        AddressNetwork::from_profile_id(&self.launch_profile.profile_id)
    }

    fn wallet_external_addresses(&self, wallet: &Wallet) -> Result<Vec<(u32, VersionedAddress)>> {
        let tip_changed = !wallet.external_discovery_tip_matches(self.tip_hash());
        let used = if tip_changed {
            self.used_hybrid_addresses()?
        } else {
            self.pending_hybrid_addresses()
        };
        let start = wallet.external_address_cursor();
        let mut index = start;
        let mut highest_used = None;
        let mut unused_run = 0_u32;
        while index < MAX_DISCOVERED_EXTERNAL_ADDRESSES {
            let address = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, index);
            if used.contains(&address) {
                highest_used = Some(index);
                unused_run = 0;
            } else {
                unused_run = unused_run.saturating_add(1);
                if unused_run >= HYBRID_EXTERNAL_ADDRESS_GAP_LIMIT {
                    let current = highest_used
                        .and_then(|used| used.checked_add(1))
                        .unwrap_or(start);
                    wallet.advance_external_address_cursor(current);
                    if tip_changed {
                        wallet.mark_external_discovery_tip(self.tip_hash());
                    }
                    return Ok((0..=current)
                        .map(|index| {
                            (
                                index,
                                wallet.hybrid_versioned_address_at(
                                    HybridAddressBranch::External,
                                    index,
                                ),
                            )
                        })
                        .collect());
                }
            }
            index = index.saturating_add(1);
        }
        bail!(
            "wallet external address discovery exceeded {MAX_DISCOVERED_EXTERNAL_ADDRESSES} addresses"
        )
    }

    fn wallet_reward_addresses(&self, wallet: &Wallet) -> Result<Vec<(u32, VersionedAddress)>> {
        let tip_changed = !wallet.reward_discovery_tip_matches(self.tip_hash());
        let spent = if tip_changed {
            self.spent_hybrid_addresses()?
        } else {
            self.pending_spent_hybrid_addresses()
        };
        let start = wallet.reward_address_cursor();
        let mut index = start;
        let mut highest_spent = None;
        let mut unspent_run = 0_u32;
        while index < MAX_DISCOVERED_EXTERNAL_ADDRESSES {
            let address = wallet.hybrid_versioned_address_at(HybridAddressBranch::Reward, index);
            if spent.contains(&address) {
                highest_spent = Some(index);
                unspent_run = 0;
            } else {
                unspent_run = unspent_run.saturating_add(1);
                if unspent_run >= HYBRID_EXTERNAL_ADDRESS_GAP_LIMIT {
                    let current = highest_spent
                        .and_then(|spent| spent.checked_add(1))
                        .unwrap_or(start);
                    wallet.advance_reward_address_cursor(current);
                    if tip_changed {
                        wallet.mark_reward_discovery_tip(self.tip_hash());
                    }
                    return Ok((0..=current)
                        .map(|index| {
                            (
                                index,
                                wallet.hybrid_versioned_address_at(
                                    HybridAddressBranch::Reward,
                                    index,
                                ),
                            )
                        })
                        .collect());
                }
            }
            index = index.saturating_add(1);
        }
        bail!(
            "wallet reward address discovery exceeded {MAX_DISCOVERED_EXTERNAL_ADDRESSES} addresses"
        )
    }

    fn used_hybrid_addresses(&self) -> Result<Vec<VersionedAddress>> {
        let mut used = self.pending_hybrid_addresses();
        let network = self.address_network();
        let domain = self.transaction_v2_domain()?;
        for block in self.chain() {
            if let Some(address) = block.reward_address.as_deref() {
                if let Ok(address) = decode_versioned_address(address, network) {
                    if address.version == super::AddressVersion::HybridKeyCommitment {
                        push_unique_address(&mut used, address);
                    }
                }
            }
            for transaction in &block.transactions {
                collect_legacy_hybrid_outputs(transaction, network, &mut used);
            }
            for envelope in &block.transactions_v2 {
                let bytes = decode_hex(envelope).context("invalid confirmed transaction-v2 hex")?;
                let (decoded_domain, transaction) = TransactionV2::decode(&bytes)?;
                if decoded_domain == domain {
                    collect_v2_output_addresses(&transaction, &mut used);
                }
            }
        }
        Ok(used)
    }

    fn pending_hybrid_addresses(&self) -> Vec<VersionedAddress> {
        let mut used = Vec::new();
        let network = self.address_network();
        for transaction in &self.pending {
            collect_legacy_hybrid_outputs(transaction, network, &mut used);
        }
        for transaction in &self.pending_v2 {
            collect_v2_output_addresses(transaction, &mut used);
        }
        used
    }

    fn spent_hybrid_addresses(&self) -> Result<Vec<VersionedAddress>> {
        let mut spent = self.pending_spent_hybrid_addresses();
        let domain = self.transaction_v2_domain()?;
        for block in self.chain() {
            for envelope in &block.transactions_v2 {
                let bytes = decode_hex(envelope).context("invalid confirmed transaction-v2 hex")?;
                let (decoded_domain, transaction) = TransactionV2::decode(&bytes)?;
                if decoded_domain == domain {
                    collect_v2_input_addresses(&transaction, &mut spent);
                }
            }
        }
        Ok(spent)
    }

    fn pending_spent_hybrid_addresses(&self) -> Vec<VersionedAddress> {
        let mut spent = Vec::new();
        for transaction in &self.pending_v2 {
            collect_v2_input_addresses(transaction, &mut spent);
        }
        spent
    }

    /// Builds one consolidation transaction from every currently spendable legacy wallet output
    /// into the wallet's hybrid address. Submission remains subject to the height-3000 gate.
    pub fn build_v2_migration(&self, wallet: &Wallet, fee: Amount) -> Result<TransactionV2> {
        let available = self.available_utxos_for_address(wallet.address())?;
        if available.is_empty() {
            bail!("no legacy outputs are available for migration");
        }
        self.build_v2_migration_from_available(wallet, fee, &available, true)
    }

    /// Builds the largest deterministic prefix of legacy outputs that fits one block.
    pub fn build_v2_migration_batch(&self, wallet: &Wallet, fee: Amount) -> Result<TransactionV2> {
        let available = self.available_utxos_for_address(wallet.address())?;
        if available.is_empty() {
            bail!("no legacy outputs are available for migration");
        }
        let mut input_count = available.len().min(1_000);
        loop {
            let transaction = self.build_v2_migration_from_available(
                wallet,
                fee,
                &available[..input_count],
                false,
            )?;
            let bytes = transaction.encoded_size_bytes(&self.transaction_v2_domain()?)?;
            if ensure_v2_transaction_within_block_budget(
                self,
                &transaction,
                &self.transaction_v2_domain()?,
                self.launch_profile.max_block_bytes,
            )
            .is_ok()
            {
                return Ok(transaction);
            }
            if input_count == 1 {
                bail!(
                    "one-input migration requires {bytes} bytes and exceeds the {}-byte block budget",
                    self.launch_profile.max_block_bytes
                );
            }
            let proportional = ((input_count as u128)
                .saturating_mul(self.launch_profile.max_block_bytes as u128)
                / bytes as u128) as usize;
            input_count = proportional.clamp(1, input_count - 1);
        }
    }

    fn build_v2_migration_from_available(
        &self,
        wallet: &Wallet,
        fee: Amount,
        available: &[(OutPoint, TxOutput)],
        enforce_block_budget: bool,
    ) -> Result<TransactionV2> {
        let mut total = 0_u64;
        let mut inputs = Vec::with_capacity(available.len());
        for (outpoint, output) in available {
            total = total
                .checked_add(output.amount)
                .context("migration input total overflows")?;
            inputs.push(TransactionV2LegacyInput {
                outpoint_id: legacy_transaction_id(&outpoint.txid)?,
                outpoint_index: outpoint.index,
                owner: wallet.legacy_versioned_address(),
            });
        }
        let migrated_amount = total
            .checked_sub(fee)
            .context("migration fee exceeds available value")?;
        if migrated_amount == 0 {
            bail!("migration output must be greater than zero");
        }

        let domain = TransactionV2Domain::new(
            self.launch_profile.profile_id.clone(),
            decode_hex_array::<32>(self.genesis_hash())
                .context("ledger genesis hash is not a 32-byte hexadecimal value")?,
        )?;
        let mut transaction = TransactionV2::Migration {
            inputs,
            outputs: vec![TransactionV2Output {
                address: wallet.hybrid_versioned_address(),
                amount: migrated_amount,
            }],
            fee,
            authorizations: Vec::new(),
        };
        let payload = transaction.signing_bytes(&domain)?;
        let authorization =
            wallet.sign_v2_authorization(wallet.legacy_versioned_address(), &payload)?;
        if let TransactionV2::Migration {
            inputs,
            authorizations,
            ..
        } = &mut transaction
        {
            authorizations.resize(inputs.len(), authorization);
        }
        transaction.verify_authorizations(&domain)?;
        if enforce_block_budget {
            ensure_v2_transaction_within_block_budget(
                self,
                &transaction,
                &domain,
                self.launch_profile.max_block_bytes,
            )?;
        }
        Ok(transaction)
    }

    pub fn build_v2_transfer(
        &self,
        wallet: &Wallet,
        recipient: VersionedAddress,
        amount: Amount,
        fee: Amount,
    ) -> Result<TransactionV2> {
        if recipient.version != super::AddressVersion::HybridKeyCommitment {
            bail!("transaction v2 recipient must use address v1");
        }
        if amount == 0 {
            bail!("transfer amount must be greater than zero");
        }
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let mut available = Vec::new();
        for owner in self.wallet_owned_hybrid_addresses(wallet)? {
            let owner_address = encode_versioned_address(owner, self.address_network())?;
            available.extend(
                self.available_utxos_for_address(&owner_address)?
                    .into_iter()
                    .map(|(outpoint, output)| (outpoint, output, owner)),
            );
        }
        available.sort_by(|(left_point, left, _), (right_point, right, _)| {
            right
                .amount
                .cmp(&left.amount)
                .then_with(|| left_point.cmp(right_point))
        });

        let mut total = 0_u64;
        let mut inputs = Vec::new();
        for (outpoint, output, owner) in available {
            total = total
                .checked_add(output.amount)
                .context("transaction v2 input total overflows")?;
            inputs.push(TransactionV2Input {
                outpoint_txid: decode_hex_array::<32>(&outpoint.txid)
                    .context("transaction v2 outpoint ID must be a 32-byte hash")?,
                outpoint_index: outpoint.index,
                owner,
            });
            if total >= required {
                break;
            }
        }
        if total < required {
            bail!("insufficient hybrid funds");
        }

        let mut outputs = vec![TransactionV2Output {
            address: recipient,
            amount,
        }];
        let change = total - required;
        if change > 0 {
            let change_address = self
                .wallet_external_addresses(wallet)?
                .last()
                .map(|(_, address)| *address)
                .context("wallet change address is unavailable")?;
            outputs.push(TransactionV2Output {
                address: change_address,
                amount: change,
            });
        }
        let domain = self.transaction_v2_domain()?;
        let mut transaction = TransactionV2::Transfer {
            inputs,
            outputs,
            fee,
            authorizations: Vec::new(),
        };
        let payload = transaction.signing_bytes(&domain)?;
        if let TransactionV2::Transfer {
            inputs,
            authorizations,
            ..
        } = &mut transaction
        {
            *authorizations = inputs
                .iter()
                .map(|input| wallet.sign_v2_authorization(input.owner, &payload))
                .collect::<Result<Vec<_>>>()?;
        }
        transaction.verify_authorizations(&domain)?;
        ensure_v2_transaction_within_block_budget(
            self,
            &transaction,
            &domain,
            self.launch_profile.max_block_bytes,
        )?;
        Ok(transaction)
    }

    pub fn build_v2_burn(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
    ) -> Result<TransactionV2> {
        self.build_v2_burn_with_anchor(wallet, amount, fee, self.tip_hash())
    }

    pub(crate) fn build_v2_burn_for_next_block(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
    ) -> Result<TransactionV2> {
        self.build_v2_burn_with_anchor(wallet, amount, fee, &self.tip().prev_hash)
    }

    fn build_v2_burn_with_anchor(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        anchor: &str,
    ) -> Result<TransactionV2> {
        if amount == 0 {
            bail!("burn amount must be greater than zero");
        }
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let mut selected = None;
        for owner in self.wallet_owned_hybrid_addresses(wallet)? {
            let owner_address = encode_versioned_address(owner, self.address_network())?;
            let mut available = self.available_utxos_for_address(&owner_address)?;
            available.sort_by(|(left_point, left), (right_point, right)| {
                right
                    .amount
                    .cmp(&left.amount)
                    .then_with(|| left_point.cmp(right_point))
            });
            let total = available.iter().try_fold(0_u64, |total, (_, output)| {
                total
                    .checked_add(output.amount)
                    .context("transaction v2 input total overflows")
            })?;
            if total >= required {
                selected = Some((owner, available));
                break;
            }
        }
        let Some((owner, available)) = selected else {
            bail!("insufficient hybrid funds in one address for burn");
        };
        let mut total = 0_u64;
        let mut inputs = Vec::new();
        for (outpoint, output) in available {
            total = total
                .checked_add(output.amount)
                .context("transaction v2 input total overflows")?;
            inputs.push(TransactionV2Input {
                outpoint_txid: decode_hex_array::<32>(&outpoint.txid)
                    .context("transaction v2 outpoint ID must be a 32-byte hash")?,
                outpoint_index: outpoint.index,
                owner,
            });
            if total >= required {
                break;
            }
        }
        if total < required {
            bail!("insufficient hybrid funds");
        }
        let change_amount = total - required;
        let change_address = self
            .wallet_external_addresses(wallet)?
            .last()
            .map(|(_, address)| *address)
            .context("wallet change address is unavailable")?;
        let change = (change_amount > 0)
            .then_some(TransactionV2Output {
                address: change_address,
                amount: change_amount,
            })
            .into_iter()
            .collect();
        let domain = self.transaction_v2_domain()?;
        let mut transaction = TransactionV2::Burn {
            inputs,
            change,
            amount,
            fee,
            anchor: Some(
                decode_hex_array::<32>(anchor).context("transaction v2 burn anchor is invalid")?,
            ),
            authorizations: Vec::new(),
        };
        let payload = transaction.signing_bytes(&domain)?;
        let authorization = wallet.sign_v2_authorization(owner, &payload)?;
        if let TransactionV2::Burn {
            inputs,
            authorizations,
            ..
        } = &mut transaction
        {
            authorizations.resize(inputs.len(), authorization);
        }
        transaction.verify_authorizations(&domain)?;
        ensure_v2_transaction_within_block_budget(
            self,
            &transaction,
            &domain,
            self.launch_profile.max_block_bytes,
        )?;
        Ok(transaction)
    }

    pub fn build_transfer(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        let to = to.into();
        validate_address(&to, "transfer recipient")?;
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let mut available = self.available_utxos_for_address(wallet.address())?;
        // Prefer the smallest sufficient single output. Otherwise minimize input count.
        if let Some((point, _)) = available
            .iter()
            .filter(|(_, output)| output.amount >= required)
            .min_by_key(|(point, output)| (output.amount, point))
        {
            return self.build_transfer_with_inputs(
                wallet,
                to,
                amount,
                fee,
                std::slice::from_ref(point),
            );
        }
        available.sort_by(|(left_point, left), (right_point, right)| {
            right
                .amount
                .cmp(&left.amount)
                .then_with(|| left_point.cmp(right_point))
        });
        let mut total = 0_u64;
        let mut points = Vec::new();
        for (point, output) in available {
            total = total
                .checked_add(output.amount)
                .context("selected input total overflows")?;
            points.push(point);
            if total >= required {
                break;
            }
        }
        if total < required {
            bail!("insufficient funds for {}", wallet.address());
        }
        self.build_transfer_with_inputs(wallet, to, amount, fee, &points)
    }

    pub fn build_transfer_with_inputs(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let to = to.into();
        validate_address(&to, "transfer recipient")?;
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let (inputs, input_total) =
            self.select_inputs_by_outpoint(wallet.address(), required, outpoints)?;
        let mut outputs = vec![TxOutput {
            address: to,
            amount,
        }];
        let change = input_total
            .checked_sub(required)
            .context("selected inputs do not cover transfer")?;
        if change > 0 {
            outputs.push(TxOutput {
                address: wallet.address().to_string(),
                amount: change,
            });
        }
        let transaction = UnsignedUtxoTransaction::Transfer {
            inputs,
            outputs,
            fee,
        }
        .sign(wallet, &self.transaction_signing_domain())?;
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn build_burn(&self, wallet: &Wallet, amount: Amount, fee: Amount) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        self.build_burn_from_inputs(
            wallet,
            amount,
            fee,
            inputs,
            input_total,
            self.public_burn_anchor(),
        )
    }

    pub fn build_burn_with_inputs(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) =
            self.select_inputs_by_outpoint(wallet.address(), required, outpoints)?;
        self.build_burn_from_inputs(
            wallet,
            amount,
            fee,
            inputs,
            input_total,
            self.public_burn_anchor(),
        )
    }

    #[cfg(test)]
    pub(crate) fn build_burn_for_next_block(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        self.build_burn_from_inputs(
            wallet,
            amount,
            fee,
            inputs,
            input_total,
            self.next_block_burn_anchor(),
        )
    }

    pub(crate) fn build_burn_for_next_block_with_inputs(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) =
            self.select_inputs_by_outpoint(wallet.address(), required, outpoints)?;
        self.build_burn_from_inputs(
            wallet,
            amount,
            fee,
            inputs,
            input_total,
            self.next_block_burn_anchor(),
        )
    }

    fn build_burn_from_inputs(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        inputs: Vec<UnsignedTxInput>,
        input_total: Amount,
        anchor: Option<String>,
    ) -> Result<Transaction> {
        let signing_height = if anchor.as_deref() == Some(self.tip_hash()) {
            self.height().saturating_add(2)
        } else {
            self.height().saturating_add(1)
        };
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let change_amount = input_total
            .checked_sub(required)
            .context("selected inputs do not cover burn")?;
        let change = if change_amount > 0 {
            vec![TxOutput {
                address: wallet.address().to_string(),
                amount: change_amount,
            }]
        } else {
            Vec::new()
        };
        let transaction = UnsignedUtxoTransaction::Burn {
            inputs,
            change,
            amount,
            fee,
            anchor,
        }
        .sign(wallet, &self.transaction_signing_domain_at(signing_height))?;
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    fn public_burn_anchor(&self) -> Option<String> {
        // Public burns enter a one-block queue: a burn signed at tip H is
        // eligible in the block after H's direct child.
        (self.height().saturating_add(2) >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT)
            .then(|| self.tip().hash.clone())
    }

    fn next_block_burn_anchor(&self) -> Option<String> {
        // A finalizer learns its role only after the parent exists, so its
        // mandatory local burn is signed directly against that parent's parent.
        (self.height().saturating_add(1) >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT)
            .then(|| self.tip().prev_hash.clone())
    }

    pub fn build_mine(&self, recipient: impl Into<String>) -> Result<Transaction> {
        let recipient = recipient.into();
        self.validate_mine_reward_address(&recipient)?;
        let anchor = self.tip().hash.clone();
        let salt = 1;
        let difficulty_bits = self.current_mine_difficulty_bits();
        let signing_domain = self.transaction_signing_domain();
        for nonce in 0..u64::MAX {
            let signature = mine_signature(
                &signing_domain,
                &recipient,
                &anchor,
                salt,
                nonce,
                difficulty_bits,
            )?;
            if !hash_meets_difficulty(&signature, difficulty_bits) {
                continue;
            }
            let transaction = Transaction::Mine {
                recipient: recipient.clone(),
                anchor: anchor.clone(),
                salt,
                nonce,
                difficulty_bits,
                proof_header: None,
                signature,
            };
            if self.has_transaction(transaction.signature()) {
                continue;
            }
            self.validate_new_transaction(&transaction)?;
            return Ok(transaction);
        }
        bail!("could not find valid mine proof");
    }

    pub fn search_mine(
        &self,
        recipient: impl Into<String>,
        salt: u64,
        start_nonce: u64,
        max_attempts: u64,
    ) -> Result<MineSearchOutcome> {
        let recipient = recipient.into();
        self.validate_mine_reward_address(&recipient)?;
        let anchor = self.tip().hash.clone();
        let difficulty_bits = self.current_mine_difficulty_bits();
        let signing_domain = self.transaction_signing_domain();
        let mut attempts = 0_u64;
        let mut nonce = start_nonce;
        while attempts < max_attempts {
            let signature = mine_signature(
                &signing_domain,
                &recipient,
                &anchor,
                salt,
                nonce,
                difficulty_bits,
            )?;
            attempts = attempts.saturating_add(1);
            let next_nonce = nonce.checked_add(1).unwrap_or(0);
            if hash_meets_difficulty(&signature, difficulty_bits) {
                let transaction = Transaction::Mine {
                    recipient: recipient.clone(),
                    anchor: anchor.clone(),
                    salt,
                    nonce,
                    difficulty_bits,
                    proof_header: None,
                    signature,
                };
                if !self.has_transaction(transaction.signature()) {
                    self.validate_new_transaction(&transaction)?;
                    return Ok(MineSearchOutcome {
                        transaction: Some(transaction),
                        next_nonce,
                        attempts,
                    });
                }
            }
            nonce = next_nonce;
        }
        Ok(MineSearchOutcome {
            transaction: None,
            next_nonce: nonce,
            attempts,
        })
    }

    pub fn stratum_mine_template(
        &self,
        recipient: impl Into<String>,
        anchor: impl AsRef<str>,
        salt: u64,
        difficulty_bits: u32,
    ) -> Result<StratumMineTemplate> {
        let recipient = recipient.into();
        self.validate_mine_reward_address(&recipient)?;
        stratum_mine_template(
            &self.transaction_signing_domain(),
            recipient,
            anchor.as_ref(),
            salt,
            difficulty_bits,
        )
    }

    fn validate_mine_reward_address(&self, recipient: &str) -> Result<()> {
        let height = self.height().saturating_add(1);
        if height < super::HYBRID_REWARD_ACTIVATION_HEIGHT {
            return validate_address(recipient, "mine recipient");
        }
        let address = super::decode_versioned_address(
            recipient,
            AddressNetwork::from_profile_id(&self.launch_profile.profile_id),
        )?;
        if address.version != super::AddressVersion::HybridKeyCommitment {
            bail!("mine reward must use a hybrid address at height {height}");
        }
        Ok(())
    }

    pub fn build_stratum_mine(
        &self,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let nonce = super::stratum::pack_stratum_nonce(share.extranonce2, share.header_nonce);
        let header = stratum_mine_header_bytes(
            &self.transaction_signing_domain(),
            &template.recipient,
            &template.anchor,
            template.salt,
            nonce,
            template.difficulty_bits,
        )?;
        let transaction = Transaction::Mine {
            recipient: template.recipient,
            anchor: template.anchor,
            salt: template.salt,
            nonce,
            difficulty_bits: template.difficulty_bits,
            proof_header: Some(hex_encode(header)),
            signature: stratum_mine_signature(&header),
        };
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }
}

fn collect_legacy_hybrid_outputs(
    transaction: &Transaction,
    network: AddressNetwork,
    used: &mut Vec<VersionedAddress>,
) {
    for output in transaction.outputs() {
        if let Ok(address) = decode_versioned_address(&output.address, network) {
            if address.version == super::AddressVersion::HybridKeyCommitment {
                push_unique_address(used, address);
            }
        }
    }
}

fn collect_v2_output_addresses(transaction: &TransactionV2, used: &mut Vec<VersionedAddress>) {
    let outputs = match transaction {
        TransactionV2::Migration { outputs, .. } | TransactionV2::Transfer { outputs, .. } => {
            outputs.as_slice()
        }
        TransactionV2::Burn { change, .. } => change.as_slice(),
        TransactionV2::Mine { recipient, .. } => {
            push_unique_address(used, *recipient);
            return;
        }
    };
    for output in outputs {
        push_unique_address(used, output.address);
    }
}

fn collect_v2_input_addresses(transaction: &TransactionV2, spent: &mut Vec<VersionedAddress>) {
    let inputs = match transaction {
        TransactionV2::Transfer { inputs, .. } | TransactionV2::Burn { inputs, .. } => {
            inputs.as_slice()
        }
        TransactionV2::Migration { .. } | TransactionV2::Mine { .. } => return,
    };
    for input in inputs {
        push_unique_address(spent, input.owner);
    }
}

fn push_unique_address(addresses: &mut Vec<VersionedAddress>, address: VersionedAddress) {
    if !addresses.contains(&address) {
        addresses.push(address);
    }
}

fn legacy_transaction_id(txid: &str) -> Result<LegacyTransactionId> {
    let bytes = decode_hex(txid).context("legacy outpoint ID is not hexadecimal")?;
    match bytes.len() {
        32 => Ok(LegacyTransactionId::Hash(
            bytes.try_into().expect("checked legacy hash length"),
        )),
        64 => Ok(LegacyTransactionId::Signature(
            bytes.try_into().expect("checked legacy signature length"),
        )),
        length => bail!("legacy outpoint ID must contain 32 or 64 bytes, got {length}"),
    }
}

fn ensure_v2_transaction_within_block_budget(
    ledger: &Ledger,
    transaction: &TransactionV2,
    domain: &TransactionV2Domain,
    max_block_bytes: usize,
) -> Result<()> {
    let transaction_bytes = transaction.encode(domain)?.len();
    if transaction_bytes > max_block_bytes {
        bail!(
            "transaction v2 requires {transaction_bytes} bytes and exceeds the {max_block_bytes}-byte block budget"
        );
    }
    ensure_transaction_v2_fits_empty_block(
        compact_block_context(ledger),
        &hex_encode(transaction.encode(domain)?),
        max_block_bytes,
    )?;
    Ok(())
}

#[cfg(test)]
mod v2_migration_tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn mine_reward_destination_switches_to_hybrid_at_3750() {
        let wallet = Wallet::from_seed("hybrid-mine-reward-wallet");
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.chain.last_mut().unwrap().height = super::super::HYBRID_REWARD_ACTIVATION_HEIGHT - 2;
        assert!(
            ledger
                .validate_mine_reward_address(wallet.address())
                .is_ok()
        );

        ledger.chain.last_mut().unwrap().height = super::super::HYBRID_REWARD_ACTIVATION_HEIGHT - 1;
        assert!(
            ledger
                .validate_mine_reward_address(wallet.address())
                .is_err()
        );
        assert!(
            ledger
                .validate_mine_reward_address(&wallet.hybrid_address(AddressNetwork::Mainnet))
                .is_ok()
        );
    }

    #[test]
    fn receive_and_change_advance_after_the_current_external_address_is_used() {
        let wallet = Wallet::from_seed("rotating-external-wallet");
        let recipient = Wallet::from_seed("rotating-external-recipient");
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        let owner = wallet.hybrid_versioned_address();
        let initial_receive = ledger.wallet_receive_address(&wallet).unwrap();
        assert_eq!(
            initial_receive,
            wallet.hybrid_address(AddressNetwork::Mainnet)
        );

        ledger.pending_v2.push(TransactionV2::Migration {
            inputs: Vec::new(),
            outputs: vec![TransactionV2Output {
                address: owner,
                amount: 100,
            }],
            fee: 0,
            authorizations: Vec::new(),
        });
        let rotated_receive = ledger.wallet_receive_address(&wallet).unwrap();
        assert_eq!(
            rotated_receive,
            wallet.hybrid_address_at(HybridAddressBranch::External, 1, AddressNetwork::Mainnet)
        );
        let restored = Wallet::from_seed("rotating-external-wallet");
        assert_eq!(
            ledger.wallet_receive_address(&restored).unwrap(),
            rotated_receive
        );

        ledger.utxos.insert(
            OutPoint {
                txid: "42".repeat(32),
                index: 0,
            },
            TxOutput {
                address: initial_receive,
                amount: 100,
            },
        );
        let transaction = ledger
            .build_v2_transfer(&wallet, recipient.hybrid_versioned_address(), 40, 2)
            .unwrap();
        let TransactionV2::Transfer {
            outputs,
            authorizations,
            ..
        } = transaction
        else {
            panic!("builder returned a non-transfer transaction");
        };
        assert_eq!(
            outputs[1].address,
            wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 1,)
        );
        assert_eq!(authorizations[0].committed_address().unwrap(), owner);
    }

    #[test]
    fn reward_address_rotates_after_its_hybrid_key_is_revealed_by_a_spend() {
        let wallet = Wallet::from_seed("rotating-reward-wallet");
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        let activation = super::super::HYBRID_REWARD_ACTIVATION_HEIGHT;

        let first = ledger.wallet_reward_address(&wallet, activation);
        assert_eq!(
            first,
            ledger.wallet_reward_address(&wallet, activation + 1_000)
        );
        let first_owner = wallet.hybrid_versioned_address_at(HybridAddressBranch::Reward, 0);
        ledger.pending_v2.push(TransactionV2::Burn {
            inputs: vec![TransactionV2Input {
                outpoint_txid: [0x42; 32],
                outpoint_index: 0,
                owner: first_owner,
            }],
            change: Vec::new(),
            amount: 1,
            fee: 1,
            anchor: Some([0x24; 32]),
            authorizations: Vec::new(),
        });
        let second = ledger.wallet_reward_address(&wallet, activation + 1);
        assert_ne!(first, second);
        let restored = Wallet::from_seed("rotating-reward-wallet");
        assert_eq!(
            ledger.wallet_reward_address(&restored, activation + 1),
            second
        );

        ledger.chain.last_mut().unwrap().height = activation;
        let owned = ledger
            .wallet_owned_hybrid_encoded_addresses(&wallet)
            .unwrap();
        assert!(owned.contains(&first));
        assert!(owned.contains(&second));
    }

    #[test]
    fn seed_recovery_discovers_used_external_addresses_across_a_gap() {
        let wallet = Wallet::from_seed("external-gap-recovery-wallet");
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        let used_after_gap = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 2);
        ledger.pending_v2.push(TransactionV2::Transfer {
            inputs: Vec::new(),
            outputs: vec![TransactionV2Output {
                address: used_after_gap,
                amount: 10,
            }],
            fee: 0,
            authorizations: Vec::new(),
        });

        let restored = Wallet::from_seed("external-gap-recovery-wallet");
        assert_eq!(
            ledger.wallet_receive_address(&restored).unwrap(),
            restored.hybrid_address_at(HybridAddressBranch::External, 3, AddressNetwork::Mainnet,)
        );
        assert!(
            ledger
                .wallet_owned_hybrid_addresses(&restored)
                .unwrap()
                .contains(&used_after_gap)
        );
    }

    #[test]
    fn migration_builder_consolidates_legacy_value_into_one_hybrid_output() {
        let wallet = Wallet::from_seed("v2-migration-builder-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);

        let transaction = ledger.build_v2_migration(&wallet, 3).unwrap();
        let TransactionV2::Migration {
            inputs,
            outputs,
            fee,
            authorizations,
        } = &transaction
        else {
            panic!("builder returned a non-migration transaction");
        };
        assert_eq!(inputs.len(), 1);
        assert!(matches!(
            inputs[0].outpoint_id,
            LegacyTransactionId::Hash(_)
        ));
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].address, wallet.hybrid_versioned_address());
        assert_eq!(outputs[0].amount, 97);
        assert_eq!(*fee, 3);
        assert_eq!(authorizations.len(), inputs.len());

        let domain = TransactionV2Domain::new(
            ledger.launch_profile.profile_id.clone(),
            decode_hex_array::<32>(ledger.genesis_hash()).unwrap(),
        )
        .unwrap();
        transaction.verify_authorizations(&domain).unwrap();
        let encoded = transaction.encode(&domain).unwrap();
        assert_eq!(
            TransactionV2::decode(&encoded).unwrap(),
            (domain, transaction)
        );
    }

    #[test]
    fn migration_builder_rejects_a_transaction_larger_than_the_block_budget() {
        let wallet = Wallet::from_seed("v2-oversized-migration-wallet");
        let profile = crate::domain::LaunchProfile {
            max_block_bytes: 1,
            ..crate::domain::LaunchProfile::default()
        };
        let ledger = Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::from([(wallet.address().to_string(), 100)]),
            Vec::new(),
            1,
            profile,
        )
        .unwrap();

        let error = ledger.build_v2_migration(&wallet, 1).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exceeds the 1-byte block budget")
        );
    }

    #[test]
    fn hybrid_transfer_builder_spends_migrated_value_and_returns_hybrid_change() {
        let wallet = Wallet::from_seed("v2-transfer-builder-wallet");
        let recipient = Wallet::from_seed("v2-transfer-builder-recipient");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        let mut migrated = ledger.clone();
        migrated.utxos = ledger
            .validated_v2_utxos_at_height(&migration, 3_000)
            .unwrap();

        let transfer = migrated
            .build_v2_transfer(&wallet, recipient.hybrid_versioned_address(), 40, 2)
            .unwrap();
        let TransactionV2::Transfer {
            inputs,
            outputs,
            fee,
            authorizations,
        } = &transfer
        else {
            panic!("builder returned a non-transfer transaction");
        };

        assert_eq!(inputs.len(), 1);
        assert_eq!(*fee, 2);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].address, recipient.hybrid_versioned_address());
        assert_eq!(outputs[0].amount, 40);
        assert_eq!(outputs[1].address, wallet.hybrid_versioned_address());
        assert_eq!(outputs[1].amount, 55);
        assert_eq!(authorizations.len(), inputs.len());
        migrated
            .validate_transaction_v2_at_height(&transfer, 3_001)
            .unwrap();
    }

    #[test]
    fn migration_batch_fits_the_real_empty_block_budget() {
        let wallet = Wallet::from_seed("v2-migration-batch-wallet");
        let profile = crate::domain::LaunchProfile {
            max_block_bytes: 2_000,
            ..crate::domain::LaunchProfile::default()
        };
        let mut ledger =
            Ledger::new_with_genesis_burns_and_profile(BTreeMap::new(), Vec::new(), 1, profile)
                .unwrap();
        ledger.utxos = (1_u64..=50)
            .map(|index| {
                (
                    OutPoint {
                        txid: format!("{index:064x}"),
                        index: 0,
                    },
                    TxOutput {
                        address: wallet.address().to_string(),
                        amount: 10_000,
                    },
                )
            })
            .collect();
        ledger.chain.last_mut().unwrap().height = 2_999;

        let transaction = ledger.build_v2_migration_batch(&wallet, 1).unwrap();
        let TransactionV2::Migration { inputs, .. } = &transaction else {
            panic!("builder returned a non-migration transaction");
        };
        assert!(!inputs.is_empty());
        assert!(inputs.len() < 50);
        ledger.submit_transaction_v2(transaction).unwrap();
    }
}
