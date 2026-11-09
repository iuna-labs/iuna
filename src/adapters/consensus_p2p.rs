//! Bounded P2P wire objects.
//!
//! Transport sessions can reuse the existing TCP supervision, peer book and line framing while
//! speaking this separate network contract. Winner-signed Ledger candidates are relayed as
//! standalone objects; the unsigned deadline fallback is built deterministically by miners.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::consensus::{
    CandidateHeader, GENESIS_HASH, Hash, HeartbeatBlock, HeartbeatHeader, NETWORK_ID,
    PROTOCOL_VERSION, Sidecar, TicketTransaction, Transaction, tagged_hash,
};

pub const MAX_WIRE_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BLOCKS_PER_MESSAGE: usize = 8;
pub const MAX_LOCATOR_HASHES: usize = 32;
pub const MAX_INVENTORY_HASHES: usize = 256;
pub const MAX_RELAY_TRANSACTIONS: usize = 1_024;
pub const MAX_RELAY_TICKETS: usize = 256;
pub const MAX_RELAY_CANDIDATES: usize = 8;
pub const MAX_SHORT_IDS: usize = 32_768;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Hello {
    pub protocol_version: u16,
    pub network_id: String,
    pub genesis_hash: Hash,
    pub height: u64,
    pub tip_hash: Hash,
}

impl Hello {
    pub fn new(height: u64, tip_hash: Hash) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_owned(),
            genesis_hash: GENESIS_HASH,
            height,
            tip_hash,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol_version != PROTOCOL_VERSION
            || self.network_id != NETWORK_ID
            || self.genesis_hash != GENESIS_HASH
        {
            bail!("peer does not speak the pinned {NETWORK_ID} protocol");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactBlock {
    pub header: HeartbeatHeader,
    pub tickets: Vec<TicketTransaction>,
    pub sidecar_header: Option<CandidateHeader>,
    pub transaction_short_ids: Vec<u64>,
}

impl CompactBlock {
    pub fn from_block(block: &HeartbeatBlock) -> Self {
        let transaction_short_ids = block
            .sidecar
            .as_ref()
            .map(|sidecar| {
                sidecar
                    .transactions
                    .iter()
                    .map(|transaction| short_transaction_id(block.hash(), transaction.id()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            header: block.header.clone(),
            tickets: block.tickets.clone(),
            sidecar_header: block.sidecar.as_ref().map(|sidecar| sidecar.header.clone()),
            transaction_short_ids,
        }
    }

    pub fn validate_shape(&self) -> Result<()> {
        if self.transaction_short_ids.len() > MAX_SHORT_IDS {
            bail!("compact block contains too many transaction short IDs");
        }
        if self.header.candidate_hash != self.sidecar_header.as_ref().map(CandidateHeader::hash) {
            bail!("compact block candidate commitment does not match its sidecar header");
        }
        if self.sidecar_header.is_none() && !self.transaction_short_ids.is_empty() {
            bail!("compact block without a sidecar contains transaction IDs");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Envelope {
    Hello(Hello),
    Inventory {
        block_hashes: Vec<Hash>,
    },
    GetBlocks {
        locator: Vec<Hash>,
        limit: u16,
    },
    Blocks {
        blocks: Vec<HeartbeatBlock>,
    },
    CompactBlock(Box<CompactBlock>),
    GetTransactions {
        block_hash: Hash,
        short_ids: Vec<u64>,
    },
    Transactions {
        transactions: Vec<Transaction>,
    },
    Tickets {
        tickets: Vec<TicketTransaction>,
    },
    Candidates {
        candidates: Vec<Sidecar>,
    },
}

impl Envelope {
    pub fn validate_shape(&self) -> Result<()> {
        match self {
            Self::Hello(hello) => hello.validate(),
            Self::Inventory { block_hashes } if block_hashes.len() > MAX_INVENTORY_HASHES => {
                bail!("inventory exceeds hash limit")
            }
            Self::GetBlocks { locator, limit }
                if locator.is_empty()
                    || locator.len() > MAX_LOCATOR_HASHES
                    || usize::from(*limit) > MAX_BLOCKS_PER_MESSAGE
                    || *limit == 0 =>
            {
                bail!("block request exceeds range limits")
            }
            Self::Blocks { blocks } if blocks.len() > MAX_BLOCKS_PER_MESSAGE => {
                bail!("block response exceeds item limit")
            }
            Self::CompactBlock(block) => block.validate_shape(),
            Self::GetTransactions { short_ids, .. } if short_ids.len() > MAX_SHORT_IDS => {
                bail!("missing-transaction request exceeds item limit")
            }
            Self::Transactions { transactions } if transactions.len() > MAX_RELAY_TRANSACTIONS => {
                bail!("transaction relay exceeds item limit")
            }
            Self::Tickets { tickets } if tickets.len() > MAX_RELAY_TICKETS => {
                bail!("ticket relay exceeds item limit")
            }
            Self::Candidates { candidates }
                if candidates.len() > MAX_RELAY_CANDIDATES
                    || candidates.iter().any(|candidate| {
                        candidate.canonical_size() > crate::consensus::MAX_LEDGER_SIDECAR_BYTES
                    }) =>
            {
                bail!("signed candidate relay exceeds limits")
            }
            _ => Ok(()),
        }
    }
}

pub fn encode_line(envelope: &Envelope) -> Result<Vec<u8>> {
    envelope.validate_shape()?;
    let mut bytes = serde_json::to_vec(envelope).context("encoding P2P envelope")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_WIRE_LINE_BYTES {
        bail!("P2P envelope exceeds wire byte limit");
    }
    Ok(bytes)
}

pub fn decode_line(line: &[u8]) -> Result<Envelope> {
    if line.is_empty() || line.len() > MAX_WIRE_LINE_BYTES {
        bail!("P2P line is empty or oversized");
    }
    let payload = line.strip_suffix(b"\n").unwrap_or(line);
    let envelope = serde_json::from_slice::<Envelope>(payload).context("decoding P2P envelope")?;
    envelope.validate_shape()?;
    Ok(envelope)
}

pub fn short_transaction_id(block_hash: Hash, transaction_id: Hash) -> u64 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&block_hash);
    preimage[32..].copy_from_slice(&transaction_id);
    let hash = tagged_hash(b"IUNA-COMPACT-TXID-v1", &preimage);
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&hash[..8]);
    u64::from_be_bytes(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{Chain, GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS, mine_child};

    #[test]
    fn hello_is_bound_to_network_and_genesis() {
        let hello = Hello::new(0, GENESIS_HASH);
        assert!(hello.validate().is_ok());
        let mut foreign = hello;
        foreign.genesis_hash = [0; 32];
        assert!(foreign.validate().is_err());
    }

    #[test]
    fn compact_block_commits_header_without_relaying_candidate_body() {
        let chain = Chain::from_genesis().unwrap();
        let (block, _) = mine_child(
            chain.tip(),
            GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,
            [41; 32],
            Vec::new(),
            None,
        )
        .unwrap();
        let compact = CompactBlock::from_block(&block);
        assert_eq!(compact.header, block.header);
        assert!(compact.sidecar_header.is_none());
        assert!(compact.transaction_short_ids.is_empty());
        assert!(compact.validate_shape().is_ok());
    }

    #[test]
    fn line_roundtrip_and_item_limits_are_enforced() {
        let envelope = Envelope::Hello(Hello::new(7, [9; 32]));
        assert_eq!(
            decode_line(&encode_line(&envelope).unwrap()).unwrap(),
            envelope
        );
        let oversized = Envelope::Inventory {
            block_hashes: vec![[1; 32]; MAX_INVENTORY_HASHES + 1],
        };
        assert!(encode_line(&oversized).is_err());
        assert!(decode_line(b"{}").is_err());
    }

    #[test]
    fn compact_short_ids_are_block_scoped_and_stable() {
        let transaction = [3; 32];
        assert_eq!(
            short_transaction_id([1; 32], transaction),
            short_transaction_id([1; 32], transaction)
        );
        assert_ne!(
            short_transaction_id([1; 32], transaction),
            short_transaction_id([2; 32], transaction)
        );
    }
}
