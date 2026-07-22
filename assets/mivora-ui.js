window.mivoraApp = function mivoraApp() {
  return {
    tab: "wallet",
    status: {},
    blocks: [],
    selectedBlock: null,
    loadingOlder: false,
    hasMoreBlocks: true,
    mempool: [],
    peers: [],
    burnAmount: 0,
    transferTo: "",
    transferAmount: 25,
    peerAddress: "",
    flash: null,
    flashTimer: null,
    lastUpdated: null,
    pollHandle: null,
    newBlockHashes: new Set(),
    newBlockTimer: null,

    init() {
      this.refresh();
      this.pollHandle = setInterval(() => this.refresh(), 5000);
    },

    async refresh() {
      try {
        const [status, blocks, mempool, peers] = await Promise.all([
          this.fetchJson("/api/status"),
          this.fetchJson("/api/blocks"),
          this.fetchJson("/api/mempool"),
          this.fetchJson("/api/peers"),
        ]);
        this.status = status;
        this.mergeFreshBlocks(blocks, { animateHead: true });
        this.mempool = mempool;
        this.peers = peers;
        this.burnAmount = status.mining?.burn_per_block ?? this.burnAmount;
        this.lastUpdated = new Date();
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async fetchJson(path) {
      const response = await fetch(path, { headers: { Accept: "application/json" } });
      if (!response.ok) {
        throw new Error(`${path} returned ${response.status}`);
      }
      return response.json();
    },

    mergeFreshBlocks(freshBlocks, options = {}) {
      const previousHeights = new Set(this.blocks.map((block) => block.height));
      const previousHead = this.blocks[0]?.height;
      const previousHeadHash = this.blocks[0]?.hash;
      const wasFollowingHead =
        !this.selectedBlock || (previousHeadHash && this.selectedBlock.hash === previousHeadHash);
      const rail = this.$refs.blockRail;
      const previousScrollWidth = rail?.scrollWidth ?? 0;
      const known = new Map(this.blocks.map((block) => [block.hash, block]));
      for (const block of freshBlocks) {
        known.set(block.hash, block);
      }
      this.blocks = Array.from(known.values()).sort((left, right) => right.height - left.height);
      const currentHead = this.blocks[0] || null;
      if (wasFollowingHead) {
        this.selectedBlock = currentHead;
      } else if (!this.selectedBlock || !known.has(this.selectedBlock.hash)) {
        this.selectedBlock = this.blocks[0] || null;
      } else {
        this.selectedBlock = known.get(this.selectedBlock.hash);
      }
      this.hasMoreBlocks = this.blocks.some((block) => block.height > 0);

      const newHeadBlocks = options.animateHead
        ? this.blocks.filter(
            (block) =>
              !previousHeights.has(block.height) &&
              (typeof previousHead !== "number" || block.height > previousHead)
          )
        : [];
      if (newHeadBlocks.length > 0) {
        this.markNewBlocks(newHeadBlocks.map((block) => block.hash));
        this.$nextTick(() =>
          this.slideNewHeadBlocks(previousScrollWidth, { force: wasFollowingHead })
        );
      }
    },

    markNewBlocks(hashes) {
      this.newBlockHashes = new Set(hashes);
      if (this.newBlockTimer) {
        clearTimeout(this.newBlockTimer);
      }
      this.newBlockTimer = setTimeout(() => {
        this.newBlockHashes = new Set();
        this.newBlockTimer = null;
      }, 650);
    },

    slideNewHeadBlocks(previousScrollWidth, options = {}) {
      const rail = this.$refs.blockRail;
      if (!rail || previousScrollWidth === 0 || (!options.force && rail.scrollLeft > 4)) return;
      const addedWidth = rail.scrollWidth - previousScrollWidth;
      if (addedWidth <= 0) return;
      rail.scrollLeft = addedWidth;
      rail.scrollTo({ left: 0, behavior: "smooth" });
    },

    selectBlock(block) {
      this.selectedBlock = block;
    },

    async loadOlderBlocks() {
      if (this.loadingOlder || !this.hasMoreBlocks || this.blocks.length === 0) return;
      const oldest = Math.min(...this.blocks.map((block) => block.height));
      if (oldest <= 0) {
        this.hasMoreBlocks = false;
        return;
      }
      this.loadingOlder = true;
      try {
        const older = await this.fetchJson(`/api/blocks?before_height=${oldest}&limit=30`);
        if (older.length === 0 || older.some((block) => block.height === 0)) {
          this.hasMoreBlocks = false;
        }
        this.mergeFreshBlocks(older);
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        this.loadingOlder = false;
      }
    },

    maybeLoadOlderBlocks(event) {
      const rail = event.currentTarget;
      const remaining = rail.scrollWidth - rail.scrollLeft - rail.clientWidth;
      if (remaining < 280) {
        this.loadOlderBlocks();
      }
    },

    async postForm(path, fields, successMessage) {
      const body = new URLSearchParams();
      for (const [key, value] of Object.entries(fields)) {
        body.set(key, value);
      }
      const response = await fetch(path, {
        method: "POST",
        headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
        body,
      });
      const payload = await response.json();
      if (!response.ok || !payload.ok) {
        throw new Error(payload.error || `${path} returned ${response.status}`);
      }
      await this.refresh();
      this.showFlash(successMessage, "success");
    },

    async saveBurn() {
      try {
        await this.postForm(
          "/api/settings/burn-per-block",
          { amount: this.burnAmount || 0 },
          `Burn rate set to ${this.burnAmount || 0} coin(s) per block`
        );
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async sendTransfer() {
      try {
        const amount = this.transferAmount || 0;
        const recipient = this.short(this.transferTo);
        await this.postForm(
          "/api/transfer",
          { to: this.transferTo, amount },
          `Queued transfer of ${amount} coin(s) to ${recipient}`
        );
        this.transferTo = "";
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async addPeer() {
      try {
        const peer = this.peerAddress;
        await this.postForm("/api/peers", { peer }, `Added peer ${peer}`);
        this.peerAddress = "";
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    showFlash(message, kind) {
      this.flash = { message, kind };
      if (this.flashTimer) {
        clearTimeout(this.flashTimer);
      }
      this.flashTimer = setTimeout(() => {
        this.flash = null;
        this.flashTimer = null;
      }, kind === "error" ? 7000 : 3500);
    },

    short(value) {
      if (!value) return "-";
      if (value.length <= 16) return value;
      return `${value.slice(0, 8)}...${value.slice(-8)}`;
    },

    blockBurned(block) {
      return block.transactions
        .filter((tx) => tx.kind === "burn")
        .reduce((sum, tx) => sum + tx.amount, 0);
    },

    blockBurnCount(block) {
      return block.transactions.filter((tx) => tx.kind === "burn").length;
    },

    blockTransferCount(block) {
      return block.transactions.filter((tx) => tx.kind === "transfer").length;
    },

    burnCountLabel(block) {
      const count = this.blockBurnCount(block);
      return `${count} burn${count === 1 ? "" : "s"}`;
    },

    transferCountLabel(block) {
      const count = this.blockTransferCount(block);
      return `${count} transfer${count === 1 ? "" : "s"}`;
    },

    isLeaderLabel() {
      if (!this.status.mining) return "-";
      return this.status.mining.wallet_is_current_leader ? "yes" : "no";
    },

    sharedHeightLabel() {
      const local = this.status.chain?.height;
      if (typeof local !== "number") return "-";
      const peerHeights = this.peers
        .filter((peer) => !peer.last_error)
        .map((peer) => peer.last_known_height)
        .filter((height) => typeof height === "number");
      if (peerHeights.length === 0) return local;
      return Math.min(local, ...peerHeights);
    },

    targetSecondsLabel() {
      const ms = this.status.mining?.vdf_target_block_ms;
      return ms ? `${Math.round(ms / 1000)}s` : "-";
    },

    olderButtonLabel() {
      if (this.loadingOlder) return "Loading";
      return this.hasMoreBlocks ? "Load older blocks" : "Genesis loaded";
    },

    lastUpdatedLabel() {
      return this.lastUpdated ? `Updated ${this.lastUpdated.toLocaleTimeString()}` : "Loading";
    },
  };
};
