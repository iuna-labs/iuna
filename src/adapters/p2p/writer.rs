use anyhow::Result;
use tokio::{io::AsyncWriteExt, net::tcp::OwnedWriteHalf};

use crate::app::GossipEnvelope;

use super::MAX_GOSSIP_LINE_BYTES;

pub(super) async fn write_payload(
    writer: &mut OwnedWriteHalf,
    payload: &[GossipEnvelope],
) -> Result<()> {
    for envelope in payload {
        write_envelope(writer, envelope).await?;
    }
    Ok(())
}

pub(super) async fn write_envelope(
    writer: &mut OwnedWriteHalf,
    envelope: &GossipEnvelope,
) -> Result<()> {
    let line = serde_json::to_string(envelope)?;
    if line.len() > MAX_GOSSIP_LINE_BYTES {
        anyhow::bail!(
            "p2p message is {} bytes, exceeding {} byte limit",
            line.len(),
            MAX_GOSSIP_LINE_BYTES
        );
    }
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

pub(super) fn byte_bounded_block_page(
    blocks: Vec<crate::domain::Block>,
) -> Vec<crate::domain::Block> {
    let mut page = Vec::new();
    let mut encoded_len = serde_json::to_vec(&GossipEnvelope::Blocks { blocks: Vec::new() })
        .expect("block envelope serialization cannot fail")
        .len();
    for block in blocks {
        let block_len = serde_json::to_vec(&block)
            .expect("block serialization cannot fail")
            .len();
        let separator_len = usize::from(!page.is_empty());
        let Some(candidate_len) = encoded_len
            .checked_add(separator_len)
            .and_then(|len| len.checked_add(block_len))
        else {
            break;
        };
        if candidate_len > MAX_GOSSIP_LINE_BYTES {
            break;
        }
        encoded_len = candidate_len;
        page.push(block);
    }
    page
}

#[cfg(test)]
mod tests {
    use crate::{
        app::GossipEnvelope,
        domain::{Block, BurnBundleSection, FinalizerMode},
    };

    use super::{MAX_GOSSIP_LINE_BYTES, byte_bounded_block_page};

    fn large_block(height: u64) -> Block {
        Block {
            height,
            prev_hash: format!("{:064x}", height.saturating_sub(1)),
            timestamp_ms: height,
            miner: "0".repeat(64),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: 0,
            vdf_output: "x".repeat(512 * 1024),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            hash: format!("{height:064x}"),
        }
    }

    #[test]
    fn block_page_is_bounded_by_wire_bytes_not_only_item_count() {
        let blocks = (1..=32).map(large_block).collect::<Vec<_>>();
        let page = byte_bounded_block_page(blocks.clone());
        let encoded = serde_json::to_vec(&GossipEnvelope::Blocks {
            blocks: page.clone(),
        })
        .unwrap();

        assert!(!page.is_empty());
        assert!(page.len() < blocks.len());
        assert!(encoded.len() <= MAX_GOSSIP_LINE_BYTES);

        let mut one_more = page;
        one_more.push(blocks[one_more.len()].clone());
        assert!(
            serde_json::to_vec(&GossipEnvelope::Blocks { blocks: one_more })
                .unwrap()
                .len()
                > MAX_GOSSIP_LINE_BYTES
        );
    }
}
