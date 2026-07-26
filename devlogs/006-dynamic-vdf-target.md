# Devlog 006: Let The Chain Aim For Ten Minutes

The sync problem was tempting to solve in the wrong place. We could make peers trust each other more, but that weakens the protocol exactly where it should be strongest.

So this change keeps VDF validation as consensus, but makes the VDF round count dynamic. Blocks still carry the exact round count they used. Nodes validate that it is the round count the chain expected for that height.

After each block, the chain looks at a rolling average of recent block times and nudges the next round count toward a 10 minute target. The nudge is small, about 10% per block, so one weird timestamp cannot throw the chain completely off.

This gives gossip and catch-up more breathing room while keeping the rule deterministic: every node can derive the same next VDF rounds from the chain it has validated.
