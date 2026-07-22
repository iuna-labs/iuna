# Devlog 007: Gossip Grows Up A Bit

The first gossip protocol was basically "push whatever just happened, and if someone is behind, throw a full snapshot at them." That worked for tiny chains, but it was too blunt.

Now peers announce both height and tip hash when a connection opens. If a node sees that a peer is behind, it sends a batch of missing blocks instead of a whole chain snapshot. The receiver still validates the blocks, including the VDF output, before importing them.

Snapshots are still useful for initial join and fallback, but normal catch-up now has a more blockchain-shaped path: ask for the missing range, validate it, apply it.

The UI also shows the last height and tip hash reported by each peer, which makes it much easier to see whether gossip is actually moving or just quietly stuck.
