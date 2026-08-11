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
