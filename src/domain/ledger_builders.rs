use super::hex::{decode_hex, decode_hex_array, hex_encode};
use super::mining::mine_signature;
use super::stratum::{
    hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature, stratum_mine_template,
};
use super::transaction::{UnsignedTxInput, UnsignedUtxoTransaction};
use super::validation::validate_address;
use super::{
    Amount, Ledger, LegacyTransactionId, MineSearchOutcome, OutPoint, StratumMineShare,
    StratumMineTemplate, Transaction, TransactionV2, TransactionV2Domain, TransactionV2LegacyInput,
    TransactionV2Output, TxOutput, Wallet,
};
use anyhow::{Context, Result, bail};

impl Ledger {
    /// Builds one consolidation transaction from every currently spendable legacy wallet output
    /// into the wallet's hybrid address. Submission remains subject to the height-3000 gate.
    pub fn build_v2_migration(&self, wallet: &Wallet, fee: Amount) -> Result<TransactionV2> {
        let available = self.available_utxos_for_address(wallet.address())?;
        if available.is_empty() {
            bail!("no legacy outputs are available for migration");
        }

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
        ensure_v2_transaction_within_block_budget(
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
        validate_address(&recipient, "mine recipient")?;
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
        validate_address(&recipient, "mine recipient")?;
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
        stratum_mine_template(
            &self.transaction_signing_domain(),
            recipient,
            anchor.as_ref(),
            salt,
            difficulty_bits,
        )
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
    Ok(())
}

#[cfg(test)]
mod v2_migration_tests {
    use std::collections::BTreeMap;

    use super::*;

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
}
