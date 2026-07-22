# Devlog 003: Friends Join The Chain

The first thought was a shared genesis file. That is fine for a lab, but it is not the friend-net experience I want.

The better flow is: I start a chain, you point your node at mine, and your node joins what I already started.

So the P2P port now does one extra friendly thing. When a node connects, the peer sends a chain snapshot: genesis allocations, VDF rounds, and the blocks it has. A joining node imports that snapshot before it starts mining. If it cannot get the snapshot, it refuses to start a separate chain.

The default burn is now zero. That matters because a friend who just joined probably has no LUUN yet. They can still follow the chain, receive LUUN, and only then decide how much to burn per block.

Genesis changed too. The starter does not begin rich anymore. The starter gets 1 synthetic Luun in genesis and burns it immediately, so their visible balance is 0, but the chain has bootstrap lottery tickets. Those tickets let the starter produce the first real reward blocks while normal burn tickets mature.

This is still not real adversarial sync. It trusts the friend you join. But for the current Luun phase, that is exactly the point: make a small network feel real first, then harden it later.
