use std::collections::BTreeMap;

use anyhow::Result;

use super::{GossipEnvelope, NodeCore};

#[derive(Debug, Default)]
pub struct InMemoryNetwork {
    nodes: BTreeMap<String, NodeCore>,
}

impl InMemoryNetwork {
    pub fn insert(&mut self, id: impl Into<String>, node: NodeCore) {
        self.nodes.insert(id.into(), node);
    }

    pub fn node(&self, id: &str) -> Option<&NodeCore> {
        self.nodes.get(id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut NodeCore> {
        self.nodes.get_mut(id)
    }

    pub fn deliver_until_idle(&mut self) -> Result<()> {
        loop {
            let mut outbound = Vec::new();
            for (id, node) in &mut self.nodes {
                for envelope in node.drain_outbox() {
                    outbound.push((id.clone(), envelope));
                }
            }

            if outbound.is_empty() {
                return Ok(());
            }

            for (from, envelope) in outbound {
                for (id, node) in &mut self.nodes {
                    if *id != from {
                        receive_in_memory_envelope(node, envelope.clone())?;
                    }
                }
            }
        }
    }

    pub fn gossip_mempools_once(&mut self) -> Result<()> {
        let mut outbound = Vec::new();
        for (id, node) in &mut self.nodes {
            for envelope in node.mempool_gossip() {
                outbound.push((id.clone(), envelope));
            }
        }

        for (from, envelope) in outbound {
            for (id, node) in &mut self.nodes {
                if *id != from {
                    receive_in_memory_envelope(node, envelope.clone())?;
                }
            }
        }
        Ok(())
    }

    pub fn sync_node_from_peer(&mut self, from: &str, to: &str, limit: usize) -> Result<bool> {
        let from_height = self
            .nodes
            .get(to)
            .map(|node| node.chain_height() + 1)
            .ok_or_else(|| anyhow::anyhow!("missing sync target node {to}"))?;
        let blocks = self
            .nodes
            .get(from)
            .map(|node| node.blocks_from(from_height, limit))
            .ok_or_else(|| anyhow::anyhow!("missing sync source node {from}"))?;
        if blocks.is_empty() {
            return Ok(false);
        }

        self.nodes
            .get_mut(to)
            .expect("sync target exists")
            .receive(GossipEnvelope::Blocks { blocks })?;
        Ok(true)
    }
}

fn receive_in_memory_envelope(node: &mut NodeCore, envelope: GossipEnvelope) -> Result<()> {
    let transaction_like = matches!(
        envelope,
        GossipEnvelope::Transaction(_) | GossipEnvelope::Transactions { .. }
    );
    match node.receive(envelope) {
        Ok(()) => Ok(()),
        Err(_) if transaction_like => Ok(()),
        Err(error) => Err(error),
    }
}
