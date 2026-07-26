# Devlog 009: Longer Fork Reorgs

Until now, iuna mostly behaved like there was only one possible chain. If a snapshot disagreed with a block we already had, the node rejected it. That is nice and simple, but it is not how a real network behaves. Two friendly nodes can still mine competing blocks if messages arrive in a weird order.

The new rule is intentionally small: a remote chain can replace the local chain only if it has the same genesis, fully validates, shares a common ancestor, and is strictly longer. Same-height forks do not cause flip-flopping. The node waits until one side grows longer.

When a reorg happens, local pending transactions are not thrown away. Transactions from abandoned local blocks are also put back through the mempool rules, so useful burns/transfers get another chance on the new tip if they are still valid.

This is not final chain-selection science yet. There is no cumulative-work score beyond height. But it is a real fork recovery path, and it gives the gossip layer something sane to do when peers briefly disagree.
