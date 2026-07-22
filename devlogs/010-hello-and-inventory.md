# Devlog 010: Hello, Inventory

The P2P protocol now starts with a real `Hello`. A node tells the peer its protocol version, network id, genesis hash, listen address, height, and tip hash. If the protocol, network, or genesis does not match, the session is rejected early.

That matters because "it connected" is not enough for a coin. A node on a different genesis should not be able to quietly trade blocks with us and create weird local errors later.

Gossip also changed. Instead of pushing full transactions and blocks every time, nodes announce inventory: transaction signatures and block hashes. Peers then request only the objects they do not have yet.

So the flow is now more like:

1. I have tx/block ids.
2. You tell me which ones you need.
3. I send the full objects.
4. You validate before importing.

It is still simple, but it is now much closer to a real P2P shape. Less duplicate payload spam, better validation boundary, and a cleaner place to add peer scoring/rate limits later.
