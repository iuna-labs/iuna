# Consensus vocabulary

Fork choice was working, but the code still read like loose booleans and hash comparisons.

This pass gives the domain language names: `ForkPoint`, `LeaderScore`, `ForkQuality`, and `ForkChoice`. The behavior stays the same, but the code now says what it means:

- find the common ancestor
- reject finalized history rewrites
- compare leader quality inside the reorg window
- decide whether to keep local or switch
- carry abandoned local transactions back into the mempool

For now `LeaderScore` is still derived from the block hash. That is a devnet stand-in for an explicit VRF proof/score, but at least the concept now has a home in the domain model.
