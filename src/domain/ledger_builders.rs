use super::hex::hex_encode;
use super::mining::mine_signature;
use super::stratum::{
    hash_meets_difficulty, stratum_mine_header_bytes, stratum_mine_signature, stratum_mine_template,
};
use super::transaction::{UnsignedTxInput, UnsignedUtxoTransaction};
use super::validation::validate_address;
use super::{
    Amount, Ledger, MineSearchOutcome, OutPoint, StratumMineShare, StratumMineTemplate,
    Transaction, TxOutput, Wallet,
};
use anyhow::{Context, Result, bail};

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
