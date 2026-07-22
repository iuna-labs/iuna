# Devlog 005: Burned Blocks And VDF

I tightened the rule that felt wrong during local testing: a normal block cannot be empty of burns anymore.

That means a block has to carry at least one positive burn transaction. Otherwise it would create a tip with no lottery tickets for the next leader, which is basically a protocol pothole.

The VDF also now runs over the candidate block content hash instead of just the previous hash. So if the leader changes the timestamp, miner, reward, rounds, previous hash, or transactions after doing the VDF, peers reject it.

One practical consequence: the default genesis still leaves the starter wallet at 0, so it creates the chain but waits. For a moving local demo, start with one extra genesis Luun and burn it into block 1.
