use anyhow::{Context, Result, anyhow, bail};
use getrandom::getrandom;

use super::blinded::{
    blinded_envelope_fee_for_transaction, blinded_payload_from_transaction,
    blinded_transaction_commitment, blinded_transaction_signing_payload, encrypt_blinded_payload,
};
use super::hex::{hex_encode, hex_hash};
use super::mining::mine_signature;
use super::stratum::{
    hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature, stratum_mine_template,
};
use super::transaction::{
    UnsignedTxInput, UnsignedUtxoTransaction, signed_blinded_inputs, unsigned_inputs,
};
use super::validation::validate_address;
use super::{
    Amount, BLINDED_KEY_BYTES, BLINDED_NONCE_BYTES, BlindedReveal, BlindedTransaction,
    BuiltBlindedTransaction, Ledger, MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MineSearchOutcome,
    OutPoint, StratumMineShare, StratumMineTemplate, Transaction, TxOutput, Wallet,
};

impl Ledger {
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
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
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
        .sign(wallet);
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
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
        .sign(wallet);
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn build_burn(&self, wallet: &Wallet, amount: Amount, fee: Amount) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        self.build_burn_from_inputs(wallet, amount, fee, inputs, input_total)
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
        self.build_burn_from_inputs(wallet, amount, fee, inputs, input_total)
    }

    fn build_burn_from_inputs(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        inputs: Vec<UnsignedTxInput>,
        input_total: Amount,
    ) -> Result<Transaction> {
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
        }
        .sign(wallet);
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn build_blinded_burn(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        let transaction = self.build_burn(wallet, amount, fee)?;
        self.blind_transaction(wallet, transaction, fee, expires_at_height)
    }

    pub fn build_blinded_transfer(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        let transaction = self.build_transfer(wallet, to, amount, fee)?;
        self.blind_transaction(wallet, transaction, fee, expires_at_height)
    }

    pub fn build_blinded_transaction(
        &self,
        wallet: &Wallet,
        transaction: Transaction,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        if matches!(transaction, Transaction::Mine { .. }) {
            bail!("mine actions are public and cannot be blinded");
        }
        let fee = blinded_envelope_fee_for_transaction(&transaction);
        self.blind_transaction(wallet, transaction, fee, expires_at_height)
    }

    fn blind_transaction(
        &self,
        wallet: &Wallet,
        transaction: Transaction,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        if expires_at_height <= self.height() {
            bail!("blinded transaction expiry must be in the future");
        }
        if expires_at_height
            > self
                .height()
                .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
        {
            bail!("blinded transaction expiry is too far in the future");
        }
        if fee != blinded_envelope_fee_for_transaction(&transaction) {
            bail!("blinded transaction fee must match plaintext transaction fee");
        }
        let unsigned_inputs = if transaction.inputs().is_empty() && transaction.fee() > 0 {
            self.select_inputs(wallet.address(), transaction.fee())?.0
        } else {
            unsigned_inputs(transaction.inputs())
        };
        if unsigned_inputs
            .iter()
            .any(|input| input.owner != wallet.address())
        {
            bail!("blinded transaction inputs must be owned by the signing wallet");
        }
        let blinded_payload = blinded_payload_from_transaction(&transaction)?;
        let plaintext = serde_json::to_vec(&blinded_payload)
            .context("failed to serialize transaction for blinded payload")?;
        let payload_hash = hex_hash(&plaintext);
        let unsigned_commit_inputs = signed_blinded_inputs(&unsigned_inputs, "");
        let payload = transaction;
        let mut key = [0_u8; BLINDED_KEY_BYTES];
        let mut nonce = [0_u8; BLINDED_NONCE_BYTES];
        getrandom(&mut key)
            .map_err(|error| anyhow!("failed to generate blinded transaction key: {error}"))?;
        getrandom(&mut nonce)
            .map_err(|error| anyhow!("failed to generate blinded transaction nonce: {error}"))?;
        let ciphertext = encrypt_blinded_payload(
            &key,
            &nonce,
            &unsigned_commit_inputs,
            fee,
            expires_at_height,
            &plaintext,
        )?;
        let encrypted_size = u32::try_from(ciphertext.len())
            .context("blinded transaction ciphertext is too large")?;
        let transaction = BlindedTransaction {
            commitment: String::new(),
            inputs: unsigned_commit_inputs,
            fee,
            encrypted_size,
            expires_at_height,
            nonce: hex_encode(nonce),
            ciphertext: hex_encode(&ciphertext),
            payload_hash,
        };
        let signature = wallet.sign_payload(&blinded_transaction_signing_payload(&transaction));
        let transaction = BlindedTransaction {
            inputs: signed_blinded_inputs(&unsigned_inputs, &signature),
            ..transaction
        };
        let commitment = blinded_transaction_commitment(&transaction)?;
        let transaction = BlindedTransaction {
            commitment: commitment.clone(),
            ..transaction
        };
        self.validate_blinded_transaction(&transaction)?;
        Ok(BuiltBlindedTransaction {
            payload,
            transaction,
            reveal: BlindedReveal {
                commitment,
                key: hex_encode(key),
            },
        })
    }

    pub fn build_mine(&self, recipient: impl Into<String>) -> Result<Transaction> {
        let recipient = recipient.into();
        validate_address(&recipient, "mine recipient")?;
        let anchor = self.tip().hash.clone();
        let salt = 1;
        let difficulty_bits = self.current_mine_difficulty_bits();
        for nonce in 0..u64::MAX {
            let signature = mine_signature(&recipient, &anchor, salt, nonce, difficulty_bits);
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
        let mut attempts = 0_u64;
        let mut nonce = start_nonce;
        while attempts < max_attempts {
            let signature = mine_signature(&recipient, &anchor, salt, nonce, difficulty_bits);
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
        stratum_mine_template(recipient, anchor.as_ref(), salt, difficulty_bits)
    }

    pub fn build_stratum_mine(
        &self,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let nonce = super::stratum::pack_stratum_nonce(share.extranonce2, share.header_nonce);
        let header = stratum_mine_header_bytes(
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
