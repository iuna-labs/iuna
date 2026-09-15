use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::{
    AddressNetwork, AddressVersion, Ledger, LegacyTransactionId, MAX_PENDING_POOL_BYTES, OutPoint,
    Transaction, TransactionSubmitOutcome, TransactionV2, TransactionV2Domain, TransactionV2Input,
    TransactionV2LegacyInput, TransactionV2Output, TxOutput, decode_hex_array,
    encode_versioned_address, ensure_transaction_v2_active, hex_encode,
};

impl Ledger {
    pub fn transaction_v2_domain(&self) -> Result<TransactionV2Domain> {
        TransactionV2Domain::new(
            self.launch_profile.profile_id.clone(),
            decode_hex_array::<32>(self.genesis_hash())
                .context("ledger genesis hash is not a 32-byte hexadecimal value")?,
        )
    }

    /// Decodes a v2 envelope and rejects a chain ID or genesis hash chosen by the sender.
    pub fn decode_transaction_v2(&self, encoded: &[u8]) -> Result<TransactionV2> {
        let (domain, transaction) = TransactionV2::decode(encoded)?;
        if domain != self.transaction_v2_domain()? {
            bail!("transaction v2 belongs to a different chain domain");
        }
        Ok(transaction)
    }

    /// Validates the v2 rules against confirmed state at an explicit candidate block height.
    /// Mempool and block code can share this method without consulting wall-clock or local config.
    pub fn validate_transaction_v2_at_height(
        &self,
        transaction: &TransactionV2,
        height: u64,
    ) -> Result<()> {
        self.validated_v2_utxos_at_height(transaction, height)?;
        Ok(())
    }

    pub fn submit_transaction_v2(
        &mut self,
        transaction: TransactionV2,
    ) -> Result<TransactionSubmitOutcome> {
        self.submit_transaction_v2_at_height(transaction, self.height().saturating_add(1))
    }

    fn submit_transaction_v2_at_height(
        &mut self,
        transaction: TransactionV2,
        height: u64,
    ) -> Result<TransactionSubmitOutcome> {
        ensure_transaction_v2_active(height)?;
        let domain = self.transaction_v2_domain()?;
        let transaction_id = transaction.transaction_id(&domain)?;
        if self
            .pending_v2
            .iter()
            .any(|pending| pending.transaction_id(&domain).ok() == Some(transaction_id))
        {
            return Ok(TransactionSubmitOutcome::AlreadyKnown);
        }
        if transaction.fee() == 0 {
            bail!("public transaction v2 fee must be greater than zero");
        }
        let transaction_bytes = transaction.encoded_size_bytes(&domain)?;
        if transaction_bytes > self.launch_profile.max_block_bytes {
            bail!("transaction v2 exceeds the maximum block byte budget");
        }
        if self.pending.len().saturating_add(self.pending_v2.len())
            >= self.launch_profile.max_pending_transactions
        {
            bail!("mempool is full");
        }
        if self
            .pending_bytes
            .saturating_add(self.pending_v2_bytes)
            .checked_add(transaction_bytes)
            .is_none_or(|bytes| bytes > MAX_PENDING_POOL_BYTES)
        {
            bail!("mempool byte limit exceeded");
        }

        let mut utxos = self.utxos_after_spendable_pending()?;
        for pending in &self.pending_v2 {
            apply_prevalidated_transaction_v2_to_utxos(
                pending,
                &domain,
                AddressNetwork::from_profile_id(&self.launch_profile.profile_id),
                &mut utxos,
            )?;
        }
        apply_transaction_v2_to_utxos(
            &transaction,
            &domain,
            AddressNetwork::from_profile_id(&self.launch_profile.profile_id),
            &mut utxos,
        )?;
        self.pending_v2.push(transaction);
        self.pending_v2_bytes = self.pending_v2_bytes.saturating_add(transaction_bytes);
        Ok(TransactionSubmitOutcome::Added)
    }

    pub(super) fn transaction_conflicts_with_pending_v2(&self, transaction: &Transaction) -> bool {
        transaction.inputs().iter().any(|legacy_input| {
            self.pending_v2.iter().any(|pending| {
                let TransactionV2::Migration { inputs, .. } = pending else {
                    return false;
                };
                inputs.iter().any(|v2_input| {
                    v2_input.outpoint_index == legacy_input.outpoint.index
                        && legacy_transaction_id_hex(&v2_input.outpoint_id)
                            == legacy_input.outpoint.txid
                })
            })
        })
    }

    pub(super) fn revalidate_pending_v2(&mut self) -> Result<()> {
        self.revalidate_pending_v2_at_height(self.height().saturating_add(1))
    }

    fn revalidate_pending_v2_at_height(&mut self, height: u64) -> Result<()> {
        if ensure_transaction_v2_active(height).is_err() {
            self.pending_v2.clear();
            self.pending_v2_bytes = 0;
            return Ok(());
        }
        let domain = self.transaction_v2_domain()?;
        let network = AddressNetwork::from_profile_id(&self.launch_profile.profile_id);
        let mut utxos = self.utxos_after_spendable_pending()?;
        let pending = std::mem::take(&mut self.pending_v2);
        self.pending_v2_bytes = 0;
        for transaction in pending {
            let bytes = transaction.encoded_size_bytes(&domain)?;
            let mut candidate_utxos = utxos.clone();
            if apply_prevalidated_transaction_v2_to_utxos(
                &transaction,
                &domain,
                network,
                &mut candidate_utxos,
            )
            .is_ok()
            {
                utxos = candidate_utxos;
                self.pending_v2.push(transaction);
                self.pending_v2_bytes = self.pending_v2_bytes.saturating_add(bytes);
            }
        }
        Ok(())
    }

    pub(super) fn validated_v2_utxos_at_height(
        &self,
        transaction: &TransactionV2,
        height: u64,
    ) -> Result<BTreeMap<OutPoint, TxOutput>> {
        ensure_transaction_v2_active(height)?;
        let domain = self.transaction_v2_domain()?;
        let mut utxos = self.utxos.clone();
        apply_transaction_v2_to_utxos(
            transaction,
            &domain,
            AddressNetwork::from_profile_id(&self.launch_profile.profile_id),
            &mut utxos,
        )?;
        Ok(utxos)
    }
}

pub(super) fn apply_transaction_v2_to_utxos(
    transaction: &TransactionV2,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    apply_transaction_v2_to_utxos_with_policy(transaction, domain, network, utxos, true)
}

fn apply_prevalidated_transaction_v2_to_utxos(
    transaction: &TransactionV2,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    apply_transaction_v2_to_utxos_with_policy(transaction, domain, network, utxos, false)
}

fn apply_transaction_v2_to_utxos_with_policy(
    transaction: &TransactionV2,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    verify_authorizations: bool,
) -> Result<()> {
    let (spent, outputs, fee) = match transaction {
        TransactionV2::Migration {
            inputs,
            outputs,
            fee,
            ..
        } => (
            spend_legacy_inputs(inputs, utxos)?,
            outputs.as_slice(),
            *fee,
        ),
        TransactionV2::Transfer {
            inputs,
            outputs,
            fee,
            ..
        } => (
            spend_v2_inputs(inputs, network, utxos)?,
            outputs.as_slice(),
            *fee,
        ),
        TransactionV2::Burn { .. } => {
            bail!("transaction v2 burns are not integrated into live burn consensus")
        }
        TransactionV2::Mine { .. } => {
            bail!("transaction v2 mining is not integrated into live proof consensus")
        }
    };

    let credited = sum_outputs(outputs)?;
    let required = credited
        .checked_add(fee)
        .context("transaction v2 output value plus fee overflows")?;
    if spent != required {
        bail!("transaction v2 input value does not equal outputs plus fee");
    }

    // Verify expensive signatures only for an untrusted candidate and only after cheap state and
    // conservation checks. Pending entries are immutable and were verified on admission.
    if verify_authorizations {
        transaction.verify_authorizations(domain)?;
    }
    let transaction_id = hex_encode(transaction.transaction_id(domain)?);
    for (index, output) in outputs.iter().enumerate() {
        let index = u32::try_from(index).context("transaction v2 output index exceeds u32")?;
        let outpoint = OutPoint {
            txid: transaction_id.clone(),
            index,
        };
        if utxos
            .insert(
                outpoint,
                TxOutput {
                    address: internal_address(output.address, network)?,
                    amount: output.amount,
                },
            )
            .is_some()
        {
            bail!("transaction v2 recreates an existing outpoint");
        }
    }
    Ok(())
}

fn spend_legacy_inputs(
    inputs: &[TransactionV2LegacyInput],
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<u64> {
    let mut spent = 0_u64;
    for input in inputs {
        let outpoint = OutPoint {
            txid: legacy_transaction_id_hex(&input.outpoint_id),
            index: input.outpoint_index,
        };
        let output = utxos
            .remove(&outpoint)
            .with_context(|| format!("transaction v2 input {} is not spendable", outpoint.id()))?;
        let expected_owner = internal_address(input.owner, AddressNetwork::Mainnet)?;
        if output.address != expected_owner {
            bail!("transaction v2 legacy input owner does not match the referenced output");
        }
        spent = spent
            .checked_add(output.amount)
            .context("transaction v2 input value overflows")?;
    }
    Ok(spent)
}

fn legacy_transaction_id_hex(transaction_id: &LegacyTransactionId) -> String {
    match transaction_id {
        LegacyTransactionId::Hash(value) => hex_encode(value),
        LegacyTransactionId::Signature(value) => hex_encode(value),
    }
}

fn spend_v2_inputs(
    inputs: &[TransactionV2Input],
    network: AddressNetwork,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<u64> {
    let mut spent = 0_u64;
    for input in inputs {
        let outpoint = OutPoint {
            txid: hex_encode(input.outpoint_txid),
            index: input.outpoint_index,
        };
        let output = utxos
            .remove(&outpoint)
            .with_context(|| format!("transaction v2 input {} is not spendable", outpoint.id()))?;
        if output.address != internal_address(input.owner, network)? {
            bail!("transaction v2 input owner does not match the referenced output");
        }
        spent = spent
            .checked_add(output.amount)
            .context("transaction v2 input value overflows")?;
    }
    Ok(spent)
}

fn sum_outputs(outputs: &[TransactionV2Output]) -> Result<u64> {
    outputs.iter().try_fold(0_u64, |total, output| {
        total
            .checked_add(output.amount)
            .context("transaction v2 output value overflows")
    })
}

fn internal_address(address: super::VersionedAddress, network: AddressNetwork) -> Result<String> {
    match address.version {
        AddressVersion::Ed25519PublicKey => Ok(hex_encode(address.payload)),
        AddressVersion::HybridKeyCommitment => encode_versioned_address(address, network),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{SignatureScheme, TransactionV2Output, Wallet};

    #[test]
    fn migration_validation_switches_at_3000_and_creates_a_hybrid_utxo() {
        let wallet = Wallet::from_seed("v2-ledger-migration-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let transaction = ledger.build_v2_migration(&wallet, 3).unwrap();

        assert!(
            ledger
                .validate_transaction_v2_at_height(&transaction, 2_999)
                .is_err()
        );
        ledger
            .validate_transaction_v2_at_height(&transaction, 3_000)
            .unwrap();

        let domain = ledger.transaction_v2_domain().unwrap();
        let transaction_id = hex_encode(transaction.transaction_id(&domain).unwrap());
        let utxos = ledger
            .validated_v2_utxos_at_height(&transaction, 3_000)
            .unwrap();
        assert_eq!(
            utxos.get(&OutPoint {
                txid: transaction_id,
                index: 0,
            }),
            Some(&TxOutput {
                address: wallet.hybrid_address(AddressNetwork::Mainnet),
                amount: 97,
            })
        );
        assert_eq!(utxos.len(), 1);
    }

    #[test]
    fn hybrid_output_requires_both_signatures_when_spent() {
        let wallet = Wallet::from_seed("v2-ledger-hybrid-spend-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        let domain = ledger.transaction_v2_domain().unwrap();
        let migration_id = migration.transaction_id(&domain).unwrap();
        let migrated_utxos = ledger
            .validated_v2_utxos_at_height(&migration, 3_000)
            .unwrap();
        let mut migrated_ledger = ledger.clone();
        migrated_ledger.utxos = migrated_utxos;

        let mut transfer = TransactionV2::Transfer {
            inputs: vec![TransactionV2Input {
                outpoint_txid: migration_id,
                outpoint_index: 0,
                owner: wallet.hybrid_versioned_address(),
            }],
            outputs: vec![TransactionV2Output {
                address: wallet.hybrid_versioned_address(),
                amount: 96,
            }],
            fee: 1,
            authorizations: Vec::new(),
        };
        let payload = transfer.signing_bytes(&domain).unwrap();
        let authorization = wallet
            .sign_v2_authorization(wallet.hybrid_versioned_address(), &payload)
            .unwrap();
        assert_eq!(
            authorization.scheme(),
            SignatureScheme::HybridEd25519MlDsa44
        );
        if let TransactionV2::Transfer { authorizations, .. } = &mut transfer {
            authorizations.push(authorization);
        }
        migrated_ledger
            .validate_transaction_v2_at_height(&transfer, 3_001)
            .unwrap();

        if let TransactionV2::Transfer { authorizations, .. } = &mut transfer {
            let public_key = authorizations[0].public_key().clone();
            let classical_signature = authorizations[0].signature().as_bytes()[..64].to_vec();
            authorizations[0] = super::super::V2SpendingAuthorization::new(
                super::super::ProtocolPublicKey::new(
                    SignatureScheme::Ed25519,
                    public_key.as_bytes()[..32].to_vec(),
                )
                .unwrap(),
                super::super::ProtocolSignature::new(SignatureScheme::Ed25519, classical_signature)
                    .unwrap(),
            )
            .unwrap();
        }
        assert!(
            migrated_ledger
                .validate_transaction_v2_at_height(&transfer, 3_001)
                .is_err()
        );
    }

    #[test]
    fn decoder_rejects_an_attacker_selected_chain_domain() {
        let wallet = Wallet::from_seed("v2-ledger-domain-wallet");
        let ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let transaction = ledger.build_v2_migration(&wallet, 1).unwrap();
        let foreign = TransactionV2Domain::new("foreign-chain", [0x44; 32]).unwrap();
        let encoded = transaction.encode(&foreign).unwrap();

        assert!(ledger.decode_transaction_v2(&encoded).is_err());
    }

    #[test]
    fn v2_mempool_enforces_activation_deduplication_and_legacy_conflicts() {
        let wallet = Wallet::from_seed("v2-ledger-mempool-wallet");
        let recipient = Wallet::from_seed("v2-ledger-mempool-recipient");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let transaction = ledger.build_v2_migration(&wallet, 3).unwrap();
        let conflicting_legacy = ledger
            .build_transfer(&wallet, recipient.address(), 90, 1)
            .unwrap();

        assert!(
            ledger
                .submit_transaction_v2_at_height(transaction.clone(), 2_999)
                .is_err()
        );
        assert!(ledger.pending_v2().is_empty());
        assert_eq!(
            ledger
                .submit_transaction_v2_at_height(transaction.clone(), 3_000)
                .unwrap(),
            TransactionSubmitOutcome::Added
        );
        assert_eq!(ledger.pending_v2().len(), 1);
        assert!(ledger.pending_v2_bytes > 0);
        assert_eq!(ledger.status().pending_transactions, 1);
        assert_eq!(
            ledger
                .submit_transaction_v2_at_height(transaction, 3_000)
                .unwrap(),
            TransactionSubmitOutcome::AlreadyKnown
        );
        assert_eq!(
            ledger
                .submit_transaction_with_outcome(conflicting_legacy)
                .unwrap(),
            TransactionSubmitOutcome::ConflictsWithPending
        );
    }

    #[test]
    fn v2_mempool_accepts_a_transfer_spending_a_pending_migration() {
        let wallet = Wallet::from_seed("v2-ledger-dependent-mempool-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        let domain = ledger.transaction_v2_domain().unwrap();
        let migration_id = migration.transaction_id(&domain).unwrap();
        ledger
            .submit_transaction_v2_at_height(migration, 3_000)
            .unwrap();

        let mut transfer = TransactionV2::Transfer {
            inputs: vec![TransactionV2Input {
                outpoint_txid: migration_id,
                outpoint_index: 0,
                owner: wallet.hybrid_versioned_address(),
            }],
            outputs: vec![TransactionV2Output {
                address: wallet.hybrid_versioned_address(),
                amount: 96,
            }],
            fee: 1,
            authorizations: Vec::new(),
        };
        let payload = transfer.signing_bytes(&domain).unwrap();
        let authorization = wallet
            .sign_v2_authorization(wallet.hybrid_versioned_address(), &payload)
            .unwrap();
        if let TransactionV2::Transfer { authorizations, .. } = &mut transfer {
            authorizations.push(authorization);
        }

        assert_eq!(
            ledger
                .submit_transaction_v2_at_height(transfer, 3_000)
                .unwrap(),
            TransactionSubmitOutcome::Added
        );
        assert_eq!(ledger.pending_v2().len(), 2);
    }

    #[test]
    fn v2_mempool_revalidation_drops_a_migration_spent_by_new_chain_state() {
        let wallet = Wallet::from_seed("v2-ledger-revalidation-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        ledger
            .submit_transaction_v2_at_height(migration, 3_000)
            .unwrap();
        assert_eq!(ledger.pending_v2().len(), 1);

        ledger.utxos.clear();
        ledger.revalidate_pending_v2_at_height(3_000).unwrap();
        assert!(ledger.pending_v2().is_empty());
        assert_eq!(ledger.pending_v2_bytes, 0);
    }

    #[test]
    fn invalid_v2_candidate_does_not_mutate_the_mempool() {
        let wallet = Wallet::from_seed("v2-ledger-invalid-candidate-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let mut migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        if let TransactionV2::Migration { authorizations, .. } = &mut migration {
            let public_key = authorizations[0].public_key().clone();
            let mut signature = authorizations[0].signature().as_bytes().to_vec();
            signature[0] ^= 1;
            authorizations[0] = super::super::V2SpendingAuthorization::new(
                public_key,
                super::super::ProtocolSignature::new(SignatureScheme::Ed25519, signature).unwrap(),
            )
            .unwrap();
        }

        assert!(
            ledger
                .submit_transaction_v2_at_height(migration, 3_000)
                .is_err()
        );
        assert!(ledger.pending_v2().is_empty());
        assert_eq!(ledger.pending_v2_bytes, 0);
    }
}
