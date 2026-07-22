# Devlog 001: Node First

Pakala started with a mountain of explanation before there was much to run. Mivora starts in the opposite direction.

The first version is a single binary: wallet, node, miner, HTTP management UI, and P2P listener all in one place. It is not trying to survive hostile internet conditions yet. It is trying to make the coin feel alive as quickly as possible.

The important design choice is the hexagonal split. The coin rules live in the domain layer. The TCP server and HTTP UI sit outside that. Because of that, tests can run a little Mivora network entirely in memory, without ports, sleeps, containers, or a pretend deployment.

The consensus sketch is intentionally small:

- burn coins into the latest block,
- use those burns as lottery tickets for the next block,
- delay the draw with a simple verifiable hash-chain VDF,
- give the selected wallet the right to mine the next block,
- forget the stake because the coins were already burned.

That gives us something real to poke at now, while leaving plenty of room to make the cryptography and networking less toy-like later.
