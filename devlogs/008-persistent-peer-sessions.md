# Devlog 008: Persistent Peer Sessions

The old P2P layer opened a fresh TCP connection for almost every little thing: send a burn, send a block, ask for status, ask for missing blocks. It was easy to write, but it made the logs noisy and the network feel twitchy. Lots of "connection reset by peer" messages were basically the sound of short-lived sockets closing at awkward moments.

The new layer keeps one outbound session per known peer. Each peer gets a bounded queue, a reconnect loop with backoff, and a simple line-based message stream. Status messages keep flowing over the same connection, and if a peer reports that it is ahead, the node asks for the missing block range on that same session.

This is still intentionally small. It is not trying to be libp2p. But it is much closer to how the coin should behave: peers stay connected, gossip is queued instead of redialed, quiet disconnects are treated as normal, and catch-up is driven by the protocol instead of a separate polling fetch path.

The important part for testing is that the node core did not become network-shaped. The session layer is still an adapter around the same `GossipEnvelope` messages, so the fast deterministic tests can keep exercising the protocol without real sockets.
