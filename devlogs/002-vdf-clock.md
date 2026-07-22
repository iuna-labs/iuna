# Devlog 002: The VDF Is The Clock

The first UI had a "mine next block" button. That was useful for proving the ledger worked, but it was the wrong feeling for Mivora.

Now the node runs by itself. Each wallet has a fixed burn amount. If that amount is above zero, once per chain height the node creates a burn transaction for that amount. Those burns become lottery tickets in the block, and the latest block's burns choose who gets to make the next block.

The important correction is that there is no exact timer like "sleep 10 minutes, then make a block." The selected leader makes the block content and then does the VDF work. When the VDF is finished, the block is gossiped. That means the VDF is the clock.

The code also had to move the VDF outside the main node lock. If the VDF is supposed to be the thing that takes real time, the UI should not freeze just because the local node is hashing. So the node prepares the block content, runs the VDF separately, and then comes back to apply and gossip the block if it still fits the local chain.

The management page is also starting to feel less like a toy console and more like a tiny node dashboard. It shows the current leader, the fixed block reward, the burn setting, recent blocks, and what peers the node knows about.

Still friendly-node land. Still deliberately simple. But the rhythm is closer to the actual coin idea now.
