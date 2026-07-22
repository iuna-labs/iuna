# Fast VDF verification

The nodes were still drifting because followers had to re-run the whole VDF for every block they imported.

That was the wrong shape. The miner should spend the delay time, but peers should be able to verify the result quickly. Otherwise a node that is one block behind has to do the same work as the miner just to catch up, and if it misses a few blocks it is basically doomed to trail behind.

This pass changes the block VDF output into a small `output:proof` receipt. Mining still does sequential work, but import checks the proof quickly. Combined with inventory/request and active catchup, peers should now catch up in seconds instead of one VDF at a time.

This is still devnet-level crypto, not a final mainnet VDF construction, but the architecture is now pointed in the right direction: slow produce, fast verify.
