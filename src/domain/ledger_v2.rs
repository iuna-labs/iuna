use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::ledger_ops::{compact_block_context, ensure_transaction_v2_fits_empty_block};
use super::{
    AddressNetwork, AddressVersion, Amount, Ledger, LegacyTransactionId, LineageOwnerValues,
    MAX_PENDING_POOL_BYTES, OutPoint, Transaction, TransactionSubmitOutcome, TransactionV2,
    TransactionV2Domain, TransactionV2Input, TransactionV2LegacyInput, TransactionV2Output,
    TxOutput, UtxoLineageRoot, attach_existing_output_lineage, decode_hex, decode_hex_array,
    encode_versioned_address, ensure_transaction_v2_active, hex_encode, newest_lineage_root,
    remove_spent_output_lineage,
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
        self.validate_transaction_v2_anchor_for_block(transaction, height)?;
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
        transaction.validate_authorization_policy_at_height(height)?;
        self.validate_transaction_v2_anchor_for_pending(&transaction)?;
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
        let encoded = transaction.encode(&domain)?;
        let transaction_bytes = encoded.len();
        let envelope = hex_encode(&encoded);
        ensure_transaction_v2_fits_empty_block(
            compact_block_context(self),
            &envelope,
            self.launch_profile.max_block_bytes,
        )?;
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

        let mut utxos = self.utxos_after_all_spendable_pending()?;
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

    pub(super) fn utxos_after_all_spendable_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos_after_spendable_pending()?;
        if self.pending_v2.is_empty() {
            return Ok(utxos);
        }

        let domain = self.transaction_v2_domain()?;
        let network = AddressNetwork::from_profile_id(&self.launch_profile.profile_id);
        for pending in &self.pending_v2 {
            apply_prevalidated_transaction_v2_to_utxos(pending, &domain, network, &mut utxos)?;
        }
        Ok(utxos)
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
            if self
                .validate_transaction_v2_anchor_for_pending(&transaction)
                .and_then(|()| transaction.validate_authorization_policy_at_height(height))
                .and_then(|()| {
                    apply_prevalidated_transaction_v2_to_utxos(
                        &transaction,
                        &domain,
                        network,
                        &mut candidate_utxos,
                    )
                })
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
        transaction.validate_authorization_policy_at_height(height)?;
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

    pub(super) fn validate_transaction_v2_anchor_for_block(
        &self,
        transaction: &TransactionV2,
        height: u64,
    ) -> Result<()> {
        if !transaction.is_burn() {
            return Ok(());
        }
        let anchor = transaction
            .burn_anchor()
            .context("transaction v2 burn is missing its chain anchor")?;
        let expected = decode_hex_array::<32>(&self.tip().prev_hash)
            .context("block grandparent hash is invalid")?;
        if height >= super::TIP_BOUND_BURN_ACTIVATION_HEIGHT && anchor != expected {
            bail!("transaction v2 burn anchor does not match the block grandparent");
        }
        Ok(())
    }

    pub(super) fn validate_transaction_v2_anchor_for_pending(
        &self,
        transaction: &TransactionV2,
    ) -> Result<()> {
        if !transaction.is_burn() {
            return Ok(());
        }
        let anchor = transaction
            .burn_anchor()
            .context("transaction v2 burn is missing its chain anchor")?;
        let tip = decode_hex_array::<32>(self.tip_hash()).context("tip hash is invalid")?;
        let parent =
            decode_hex_array::<32>(&self.tip().prev_hash).context("tip parent hash is invalid")?;
        if anchor != tip && anchor != parent {
            return Err(super::ValidationError::BurnAnchorOutsidePendingWindow.into());
        }
        Ok(())
    }

    pub(crate) fn transaction_v2_is_eligible_for_next_block(
        &self,
        transaction: &TransactionV2,
    ) -> bool {
        self.validate_transaction_v2_anchor_for_block(transaction, self.height().saturating_add(1))
            .is_ok()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_transaction_v2_with_lineage(
    transaction: &TransactionV2,
    domain: &TransactionV2Domain,
    network: AddressNetwork,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    utxo_lineage: &mut BTreeMap<OutPoint, UtxoLineageRoot>,
    lineage_values: &mut BTreeMap<UtxoLineageRoot, Amount>,
    lineage_owners: &mut LineageOwnerValues,
    verify_authorizations: bool,
) -> Result<()> {
    let spent = transaction_v2_outpoints(transaction);
    let spent_outputs = spent
        .iter()
        .map(|outpoint| {
            utxos
                .get(outpoint)
                .cloned()
                .map(|output| (outpoint.clone(), output))
                .with_context(|| format!("transaction v2 input {} is not spendable", outpoint.id()))
        })
        .collect::<Result<Vec<_>>>()?;

    apply_transaction_v2_to_utxos_with_policy(
        transaction,
        domain,
        network,
        utxos,
        verify_authorizations,
    )?;

    let mut inherited_root = None;
    for (outpoint, output) in &spent_outputs {
        let root = remove_spent_output_lineage(
            outpoint,
            output,
            utxo_lineage,
            lineage_values,
            lineage_owners,
        )?;
        inherited_root = newest_lineage_root(inherited_root, root);
    }
    if let Some(root) = inherited_root {
        let transaction_id = hex_encode(transaction.transaction_id(domain)?);
        for (index, output) in transaction_v2_outputs(transaction).iter().enumerate() {
            let outpoint = OutPoint {
                txid: transaction_id.clone(),
                index: u32::try_from(index).context("transaction v2 output index exceeds u32")?,
            };
            let internal = utxos
                .get(&outpoint)
                .context("transaction v2 output is missing after application")?;
            attach_existing_output_lineage(
                outpoint,
                internal,
                root.clone(),
                utxo_lineage,
                lineage_values,
                lineage_owners,
            )?;
            debug_assert_eq!(internal.amount, output.amount);
        }
    }
    Ok(())
}

pub(super) fn decode_canonical_transaction_v2_envelope(
    envelope: &str,
    expected_domain: &TransactionV2Domain,
) -> Result<TransactionV2> {
    let bytes = decode_hex(envelope).context("transaction v2 envelope is not hexadecimal")?;
    if hex_encode(&bytes) != envelope {
        bail!("transaction v2 envelope is not canonical lowercase hexadecimal");
    }
    let (domain, transaction) = TransactionV2::decode(&bytes)?;
    if &domain != expected_domain {
        bail!("transaction v2 belongs to a different chain domain");
    }
    if transaction.encode(expected_domain)? != bytes {
        bail!("transaction v2 envelope is not canonically encoded");
    }
    Ok(transaction)
}

fn transaction_v2_outpoints(transaction: &TransactionV2) -> Vec<OutPoint> {
    match transaction {
        TransactionV2::Migration { inputs, .. } => inputs
            .iter()
            .map(|input| OutPoint {
                txid: legacy_transaction_id_hex(&input.outpoint_id),
                index: input.outpoint_index,
            })
            .collect(),
        TransactionV2::Transfer { inputs, .. } | TransactionV2::Burn { inputs, .. } => inputs
            .iter()
            .map(|input| OutPoint {
                txid: hex_encode(input.outpoint_txid),
                index: input.outpoint_index,
            })
            .collect(),
        TransactionV2::Mine { .. } => Vec::new(),
    }
}

fn transaction_v2_outputs(transaction: &TransactionV2) -> &[TransactionV2Output] {
    match transaction {
        TransactionV2::Migration { outputs, .. } | TransactionV2::Transfer { outputs, .. } => {
            outputs
        }
        TransactionV2::Burn { change, .. } => change,
        TransactionV2::Mine { .. } => &[],
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

pub(super) fn apply_prevalidated_transaction_v2_to_utxos(
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
    let (spent, outputs, fee, burned) = match transaction {
        TransactionV2::Migration {
            inputs,
            outputs,
            fee,
            ..
        } => (
            spend_legacy_inputs(inputs, utxos)?,
            outputs.as_slice(),
            *fee,
            0,
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
            0,
        ),
        TransactionV2::Burn {
            inputs,
            change,
            amount,
            fee,
            ..
        } => {
            transaction.burn_legacy_owner()?;
            (
                spend_v2_inputs(inputs, network, utxos)?,
                change.as_slice(),
                *fee,
                *amount,
            )
        }
        TransactionV2::Mine { .. } => {
            bail!("transaction v2 mining is not integrated into live proof consensus")
        }
    };

    let credited = sum_outputs(outputs)?;
    let required = credited
        .checked_add(fee)
        .context("transaction v2 output value plus fee overflows")?
        .checked_add(burned)
        .context("transaction v2 output value plus burn overflows")?;
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
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::domain::{
        GenesisBurn, LaunchProfile, SignatureScheme, TRANSACTION_V2_ACTIVATION_HEIGHT,
        TransactionV2Output, Wallet,
    };

    fn set_next_height(ledger: &mut Ledger, next_height: u64) {
        ledger.chain.last_mut().unwrap().height = next_height.saturating_sub(1);
        for ticket in &mut ledger.tickets {
            ticket.eligible_from_height = next_height;
            ticket.eligible_until_height = next_height;
        }
    }

    fn post_activation_height() -> u64 {
        TRANSACTION_V2_ACTIVATION_HEIGHT.unwrap().saturating_add(1)
    }

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
    fn hybrid_wallet_can_build_select_and_create_a_ticket_from_a_v2_burn() {
        let wallet = Wallet::from_seed("v2-burn-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        ledger.utxos = ledger
            .validated_v2_utxos_at_height(&migration, 3_000)
            .unwrap();
        let split = ledger
            .build_v2_transfer(&wallet, wallet.hybrid_versioned_address(), 40, 1)
            .unwrap();
        ledger.utxos = ledger.validated_v2_utxos_at_height(&split, 3_000).unwrap();
        set_next_height(&mut ledger, post_activation_height());

        let burn = ledger.build_v2_burn_for_next_block(&wallet, 9, 5).unwrap();
        assert_eq!(
            burn.burn_legacy_owner().unwrap().as_deref(),
            Some(wallet.address())
        );
        assert_eq!(burn.amount(), 9);
        ledger.submit_transaction_v2(burn.clone()).unwrap();
        let low_fee_burn = ledger.build_v2_burn_for_next_block(&wallet, 7, 1).unwrap();
        ledger.submit_transaction_v2(low_fee_burn.clone()).unwrap();

        let bundles = ledger.build_burn_bundles(&wallet).unwrap();
        assert!(!bundles.is_empty());
        let domain = ledger.transaction_v2_domain().unwrap();
        let expected_burns = vec![
            hex_encode(burn.encode(&domain).unwrap()),
            hex_encode(low_fee_burn.encode(&domain).unwrap()),
        ];
        assert!(
            bundles
                .iter()
                .all(|bundle| bundle.burns_v2 == vec![expected_burns[0].clone()])
        );
        let recovery = ledger
            .prepare_recovery_block_with_required_burn_and_burn_bundles(
                wallet.address(),
                None,
                ledger.recovery_block_min_timestamp(),
                Vec::new(),
                None,
            )
            .unwrap();
        assert_eq!(recovery.transactions_v2.len(), 2);
        let section = ledger.burn_bundle_section_from_bundles(bundles).unwrap();
        assert_eq!(section.burns_v2.len(), 1);
        let sorted_section = ledger
            .burn_bundle_section_from_bundles(vec![super::super::BurnBundle {
                height: ledger.height() + 1,
                prev_hash: ledger.tip_hash().to_string(),
                slot: 1,
                member: wallet.address().to_string(),
                reward_address: None,
                burns: Vec::new(),
                burns_v2: vec![expected_burns[1].clone(), expected_burns[0].clone()],
                signature: "test-signature".to_string(),
            }])
            .unwrap();
        assert_eq!(
            sorted_section.expand(ledger.height() + 1, ledger.tip_hash())[0].burns_v2,
            expected_burns
        );

        let selection = ledger
            .select_block_transactions_with_required_burn_owner(
                Some(wallet.address()),
                None,
                super::super::FinalizerMode::Ticket,
                &section,
            )
            .unwrap();
        assert!(selection.transactions.is_empty());
        assert_eq!(selection.transactions_v2.len(), 2);

        let mut block = ledger.tip().clone();
        block.height = post_activation_height();
        block.transactions.clear();
        block.transactions_v2 = selection.transactions_v2;
        let tickets =
            super::super::ticket::tickets_created_by_block(&block, ledger.launch_profile())
                .unwrap();
        assert_eq!(tickets.len(), 2);
        assert_eq!(tickets[0].owner, wallet.address());
        assert_eq!(tickets[0].amount, 9);
        assert_eq!(tickets[1].owner, wallet.address());
        assert_eq!(tickets[1].amount, 7);
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
    fn available_utxos_include_pending_v2_state() {
        let wallet = Wallet::from_seed("v2-ledger-available-utxos-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        ledger
            .submit_transaction_v2_at_height(migration, 3_000)
            .unwrap();

        assert!(
            ledger
                .available_utxos_for_address(wallet.address())
                .unwrap()
                .is_empty()
        );
        let hybrid = ledger
            .available_utxos_for_address(&wallet.hybrid_address(AddressNetwork::Mainnet))
            .unwrap();
        assert_eq!(hybrid.len(), 1);
        assert_eq!(hybrid[0].1.amount, 97);

        let wallet_addresses = BTreeSet::from([
            wallet.address().to_string(),
            wallet.hybrid_address(AddressNetwork::Mainnet),
        ]);
        let snapshot_utxos = ledger
            .available_utxos_for_addresses(&wallet_addresses)
            .unwrap();
        assert_eq!(snapshot_utxos, hybrid);
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

    #[test]
    fn v2_mempool_rejects_an_envelope_that_cannot_fit_with_block_overhead() {
        let wallet = Wallet::from_seed("v2-empty-block-budget-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        let domain = ledger.transaction_v2_domain().unwrap();
        ledger.launch_profile.max_block_bytes = migration.encoded_size_bytes(&domain).unwrap();

        assert!(
            ledger
                .submit_transaction_v2_at_height(migration, 3_000)
                .unwrap_err()
                .to_string()
                .contains("exceeds max block size")
        );
        assert!(ledger.pending_v2().is_empty());
    }

    #[test]
    fn block_selection_includes_v2_transactions_at_activation() {
        let wallet = Wallet::from_seed("v2-block-selection-wallet");
        let legacy_sender = Wallet::from_seed("v2-block-selection-legacy-sender");
        let mut ledger = Ledger::new(
            BTreeMap::from([
                (wallet.address().to_string(), 100),
                (legacy_sender.address().to_string(), 100),
            ]),
            1,
        );
        ledger.chain[0].height = 2_999;
        let migration = ledger.build_v2_migration(&wallet, 50).unwrap();
        let legacy = ledger
            .build_transfer(&legacy_sender, wallet.address(), 50, 1)
            .unwrap();
        ledger.submit_transaction(legacy).unwrap();
        ledger.submit_transaction_v2(migration.clone()).unwrap();
        ledger.launch_profile.max_block_transactions = 1;

        let selection = ledger
            .select_block_transactions_with_required_burn_owner(
                None,
                None,
                super::super::FinalizerMode::Ticket,
                &super::super::BurnBundleSection::default(),
            )
            .unwrap();

        assert!(selection.transactions.is_empty());
        assert_eq!(selection.transactions_v2.len(), 1);
        let encoded = decode_hex(&selection.transactions_v2[0]).unwrap();
        assert_eq!(ledger.decode_transaction_v2(&encoded).unwrap(), migration);
    }

    #[test]
    fn block_application_preserves_legacy_output_lineage_through_migration() {
        let wallet = Wallet::from_seed("v2-block-lineage-wallet");
        let mut ledger = Ledger::new(BTreeMap::from([(wallet.address().to_string(), 100)]), 1);
        let migration = ledger.build_v2_migration(&wallet, 3).unwrap();
        let spent = ledger.utxos.keys().next().unwrap().clone();
        let root = UtxoLineageRoot {
            outpoint: spent.clone(),
            height: 1,
        };
        ledger.utxo_lineage.insert(spent.clone(), root.clone());
        ledger.lineage_values.insert(root.clone(), 100);
        ledger.lineage_owners.insert(
            root.clone(),
            BTreeMap::from([(wallet.address().to_string(), BTreeMap::from([(spent, 100)]))]),
        );
        let domain = ledger.transaction_v2_domain().unwrap();

        apply_transaction_v2_with_lineage(
            &migration,
            &domain,
            AddressNetwork::Mainnet,
            &mut ledger.utxos,
            &mut ledger.utxo_lineage,
            &mut ledger.lineage_values,
            &mut ledger.lineage_owners,
            true,
        )
        .unwrap();

        assert_eq!(
            ledger.utxo_lineage.values().collect::<Vec<_>>(),
            vec![&root]
        );
        assert_eq!(ledger.lineage_values.get(&root), Some(&97));
        assert_eq!(
            ledger
                .lineage_owners
                .get(&root)
                .and_then(|owners| owners.get(&wallet.hybrid_address(AddressNetwork::Mainnet)))
                .map(|outputs| outputs.values().copied().sum::<u64>()),
            Some(97)
        );
    }

    #[test]
    fn height_3000_block_selects_applies_and_rewards_a_v2_migration() {
        let finalizer = Wallet::from_seed("v2-height-3000-finalizer");
        let migrator = Wallet::from_seed("v2-height-3000-migrator");
        let mut ledger = Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::from([
                (finalizer.address().to_string(), 100),
                (migrator.address().to_string(), 100),
            ]),
            vec![GenesisBurn::new(finalizer.address(), 10)],
            1,
            LaunchProfile::local_testnet(),
        )
        .unwrap();
        ledger.chain[0].height = 2_999;
        for ticket in &mut ledger.tickets {
            ticket.eligible_from_height = 3_000;
            ticket.eligible_until_height = 3_000;
        }
        let anchor = ledger.build_burn_for_next_block(&finalizer, 1, 1).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let migration = ledger.build_v2_migration(&migrator, 3).unwrap();
        ledger.submit_transaction_v2(migration.clone()).unwrap();
        let timestamp_ms = ledger.tip().timestamp_ms.saturating_add(1);

        let prepared = ledger
            .prepare_next_block(finalizer.address(), timestamp_ms)
            .unwrap();
        assert_eq!(prepared.transactions_v2.len(), 1);
        let block = prepared.finish(&finalizer, "preverified-vdf".to_string());
        assert_eq!(block.reward, 4);
        ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

        assert_eq!(ledger.height(), 3_000);
        assert_eq!(
            ledger.balance_of(&migrator.hybrid_address(AddressNetwork::Testnet)),
            97
        );
        assert!(ledger.pending_v2().is_empty());
    }

    #[test]
    fn normal_post_activation_block_flow_shares_the_transaction_count_budget() {
        let finalizer = Wallet::from_seed("v2-post-activation-count-finalizer");
        let recipient = Wallet::from_seed("v2-post-activation-count-recipient");
        let migrators = (0..4)
            .map(|index| Wallet::from_seed(&format!("v2-post-activation-count-{index}")))
            .collect::<Vec<_>>();
        let legacy_senders = (0..2)
            .map(|index| Wallet::from_seed(&format!("legacy-post-activation-count-{index}")))
            .collect::<Vec<_>>();
        let mut allocations = BTreeMap::from([
            (finalizer.address().to_string(), 100),
            (recipient.address().to_string(), 100),
        ]);
        for wallet in migrators.iter().chain(&legacy_senders) {
            allocations.insert(wallet.address().to_string(), 100);
        }
        let mut ledger = Ledger::new_with_genesis_burns_and_profile(
            allocations,
            vec![GenesisBurn::new(finalizer.address(), 10)],
            1,
            LaunchProfile::local_testnet(),
        )
        .unwrap();
        let next_height = post_activation_height();
        set_next_height(&mut ledger, next_height);
        ledger.launch_profile.max_block_transactions = 4;

        let anchor = ledger.build_burn_for_next_block(&finalizer, 1, 1).unwrap();
        ledger.submit_transaction(anchor.clone()).unwrap();
        for (index, wallet) in migrators.iter().enumerate() {
            let migration = ledger
                .build_v2_migration(wallet, 10 + index as u64)
                .unwrap();
            ledger.submit_transaction_v2(migration).unwrap();
        }
        for wallet in &legacy_senders {
            let transfer = ledger
                .build_transfer(wallet, recipient.address(), 10, 1)
                .unwrap();
            ledger.submit_transaction(transfer).unwrap();
        }

        let prepared = ledger
            .prepare_next_block(
                finalizer.address(),
                ledger.tip().timestamp_ms.saturating_add(1),
            )
            .unwrap();
        assert_eq!(
            prepared.transactions.len() + prepared.transactions_v2.len(),
            ledger.launch_profile.max_block_transactions
        );
        assert!(
            prepared
                .transactions
                .iter()
                .any(|transaction| transaction.signature() == anchor.signature())
        );
        assert!(!prepared.transactions_v2.is_empty());
        let block = prepared.finish(&finalizer, "preverified-vdf".to_string());
        ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

        assert_eq!(ledger.height(), next_height);
        assert!(
            ledger.pending().len() + ledger.pending_v2().len() > 0,
            "transactions over the shared block limit must remain pending"
        );
    }

    #[test]
    fn normal_post_activation_v2_block_enforces_the_exact_byte_boundary() {
        let finalizer = Wallet::from_seed("v2-post-activation-bytes-finalizer");
        let migrator = Wallet::from_seed("v2-post-activation-bytes-migrator");
        let mut ledger = Ledger::new_with_genesis_burns_and_profile(
            BTreeMap::from([
                (finalizer.address().to_string(), 100),
                (migrator.address().to_string(), 100),
            ]),
            vec![GenesisBurn::new(finalizer.address(), 10)],
            1,
            LaunchProfile::local_testnet(),
        )
        .unwrap();
        let next_height = post_activation_height();
        set_next_height(&mut ledger, next_height);
        let anchor = ledger.build_burn_for_next_block(&finalizer, 1, 1).unwrap();
        ledger.submit_transaction(anchor).unwrap();
        let migration = ledger.build_v2_migration(&migrator, 3).unwrap();
        ledger.submit_transaction_v2(migration).unwrap();
        let prepared = ledger
            .prepare_next_block(
                finalizer.address(),
                ledger.tip().timestamp_ms.saturating_add(1),
            )
            .unwrap();
        assert_eq!(prepared.transactions_v2.len(), 1);
        let block = prepared.finish(&finalizer, "preverified-vdf".to_string());
        let block_bytes = ledger.consensus_block_size_bytes(&block).unwrap();

        let mut exact = ledger.clone();
        exact.launch_profile.max_block_bytes = block_bytes;
        exact
            .apply_preverified_block_at(block.clone(), u64::MAX)
            .expect("post-activation v2 block at the byte limit should validate");

        let mut one_byte_over = ledger;
        one_byte_over.launch_profile.max_block_bytes = block_bytes.saturating_sub(1);
        assert!(
            one_byte_over
                .apply_preverified_block_at(block, u64::MAX)
                .is_err(),
            "post-activation v2 block one byte over the limit validated"
        );
    }
}
