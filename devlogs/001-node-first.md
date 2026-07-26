# Devlog 001: Node First

iuna starts from a running node first, then lets the explanation grow around the code.

The first version is a single binary: wallet, node, miner, HTTP management UI, and P2P listener all in one place. It is not trying to survive hostile internet conditions yet. It is trying to make the iuna feel alive as quickly as possible.

The important design choice is the hexagonal split. The iuna rules live in the domain layer. The TCP server and HTTP UI sit outside that. Because of that, tests can run a little iuna network entirely in memory, without ports, sleeps, containers, or a pretend deployment.

The consensus sketch is intentionally small:

- burn IUNA into a block,
- turn those burns into mature one-shot tickets for a short future block window,
- use parent-bound VDF work as the pacing signal,
- give the selected ticket owner the signed right to mine the next block,
- forget the stake because the IUNA were already burned.

That gives us something real to poke at now, while leaving plenty of room to make the cryptography and networking less toy-like later.
