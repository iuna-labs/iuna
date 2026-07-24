window.luunApp = function luunApp() {
  return {
    tab: "wallet",
    status: {},
    blocks: [],
    selectedBlock: null,
    selectedTransaction: null,
    loadingOlder: false,
    hasMoreBlocks: true,
    walletTxs: [],
    walletUtxos: [],
    mempool: [],
    peers: [],
    p2pMetrics: {},
    config: { setup_complete: false },
    setupWallet: { address: null, seed_phrase: null, dev_verify_bypass: false },
    setupWalletMode: "create",
    setupSeedStep: "write",
    generatedSeedPhrase: "",
    verifyChallenges: [],
    verifyAnswers: {},
    importSeedPhrase: "",
    walletVerified: false,
    setupFeedback: null,
    burnAmount: 0,
    burnAmountDraft: 0,
    burnFee: 1000000,
    burnFeeDraft: "1",
    burnAmountDirty: false,
    transferTo: "",
    transferAmount: null,
    transferFee: "1",
    showSendAdvanced: false,
    selectedTransferUtxos: [],
    peerAddress: "",
    flash: null,
    flashTimer: null,
    showWalletUtxos: false,
    lastUpdated: null,
    pollHandle: null,
    newBlockHashes: new Set(),
    newBlockTimer: null,
    blockPageSize: 20,

    init() {
      this.bootstrap();
    },

    async bootstrap() {
      await this.refreshConfig();
      if (!this.config.setup_complete) {
        await this.refreshWalletSetup();
      }
      this.tab = this.tabFromHash();
      window.addEventListener("hashchange", () => {
        this.tab = this.tabFromHash();
      });
      await this.refresh();
      this.pollHandle = setInterval(() => this.refresh(), 5000);
    },

    tabFromHash() {
      const hash = window.location.hash.replace(/^#\/?/, "");
      return ["wallet", "mining", "p2p", "chain"].includes(hash) ? hash : "wallet";
    },

    setTab(tab) {
      if (!["wallet", "mining", "p2p", "chain"].includes(tab)) return;
      this.tab = tab;
      if (window.location.hash !== `#${tab}`) {
        window.location.hash = tab;
      }
    },

    pageTitle() {
      return {
        wallet: "Luun",
        mining: "Mining",
        p2p: "P2P",
        chain: "Chain",
      }[this.tab] || "Luun";
    },

    showingSetup() {
      return !this.config.setup_complete;
    },

    async refreshConfig() {
      this.config = await this.fetchJson("/api/config");
    },

    async refreshWalletSetup() {
      const payload = await this.fetchJson("/api/wallet/setup");
      if (!payload.ok) {
        throw new Error(payload.error || "Could not load wallet setup");
      }
      this.setupWallet = payload;
      if (
        payload.seed_phrase &&
        payload.seed_phrase !== this.generatedSeedPhrase &&
        this.setupWalletMode === "create" &&
        !this.walletVerified
      ) {
        this.generatedSeedPhrase = payload.seed_phrase;
        this.walletVerified = false;
        this.setupSeedStep = "write";
        this.verifyChallenges = [];
        this.verifyAnswers = {};
      }
    },

    setupSeedWords() {
      return this.generatedSeedPhrase ? this.generatedSeedPhrase.split(/\s+/) : [];
    },

    setupAddress() {
      return this.setupWallet.address || this.status.wallet_address || "-";
    },

    selectSetupWalletMode(mode) {
      this.setupWalletMode = mode;
      this.walletVerified = mode === "import" ? this.walletVerified && !this.generatedSeedPhrase : false;
      this.setupFeedback = null;
    },

    async generateSetupSeed() {
      try {
        this.setupFeedback = null;
        const payload = await this.postWalletSetup("/api/wallet/generate", {});
        this.setupWallet = payload;
        this.generatedSeedPhrase = payload.seed_phrase || "";
        this.setupWalletMode = "create";
        this.setupSeedStep = "write";
        this.walletVerified = false;
        this.verifyChallenges = [];
        this.verifyAnswers = {};
        await this.refresh();
      } catch (error) {
        this.showSetupFeedback(error.message, "error");
      }
    },

    beginSeedVerification() {
      this.setupFeedback = null;
      const words = this.setupSeedWords();
      if (words.length < 4) {
        this.showSetupFeedback("Generate a recovery phrase first", "error");
        return;
      }
      const positions = words.map((_, index) => index);
      for (let index = positions.length - 1; index > 0; index -= 1) {
        const swapIndex = Math.floor(Math.random() * (index + 1));
        [positions[index], positions[swapIndex]] = [positions[swapIndex], positions[index]];
      }
      this.verifyChallenges = positions
        .slice(0, 4)
        .sort((left, right) => left - right)
        .map((index) => ({ index, position: index + 1 }));
      this.verifyAnswers = {};
      for (const challenge of this.verifyChallenges) {
        this.verifyAnswers[challenge.index] = "";
      }
      this.setupSeedStep = "verify";
    },

    verifyGeneratedSeed() {
      const words = this.setupSeedWords();
      const ok = this.verifyChallenges.every((challenge) => {
        const expected = words[challenge.index] || "";
        const actual = (this.verifyAnswers[challenge.index] || "").trim().toLowerCase();
        return actual === expected;
      });
      if (!ok) {
        this.showSetupFeedback("Seed word check failed", "error");
        return;
      }
      this.walletVerified = true;
      this.setupSeedStep = "verified";
      this.showSetupFeedback("Recovery phrase verified", "success");
    },

    skipSeedVerificationForDev() {
      if (!this.setupWallet.dev_verify_bypass) return;
      this.walletVerified = true;
      this.setupSeedStep = "verified";
      this.showSetupFeedback("Recovery phrase verification skipped", "success");
    },

    async importSetupSeed() {
      try {
        this.setupFeedback = null;
        const payload = await this.postWalletSetup("/api/wallet/import", {
          seed_phrase: this.importSeedPhrase,
        });
        this.setupWallet = payload;
        this.generatedSeedPhrase = "";
        this.verifyChallenges = [];
        this.verifyAnswers = {};
        this.walletVerified = true;
        this.setupSeedStep = "verified";
        await this.refresh();
        this.showSetupFeedback("Recovery phrase imported", "success");
      } catch (error) {
        this.showSetupFeedback(error.message, "error");
      }
    },

    async postWalletSetup(path, fields) {
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
      return payload;
    },

    async completeSetup() {
      try {
        if (!this.walletVerified) {
          throw new Error("Verify or import a recovery phrase first");
        }
        const response = await fetch("/api/config", {
          method: "POST",
          headers: {
            Accept: "application/json",
            "Content-Type": "application/x-www-form-urlencoded",
          },
          body: new URLSearchParams({ setup_complete: "true" }),
        });
        const payload = await response.json();
        if (!response.ok || !payload.ok) {
          throw new Error(payload.error || `/api/config returned ${response.status}`);
        }
        await this.refreshConfig();
        this.setupFeedback = null;
        this.generatedSeedPhrase = "";
        this.importSeedPhrase = "";
        this.verifyChallenges = [];
        this.verifyAnswers = {};
        this.showFlash("Setup complete", "success");
        this.setTab("wallet");
      } catch (error) {
        this.showSetupFeedback(error.message, "error");
      }
    },

    async refresh() {
      try {
        const [config, status, blocks, walletTxs, walletUtxos, mempool, peers, p2pMetrics] = await Promise.all([
          this.fetchJson("/api/config"),
          this.fetchJson("/api/status"),
          this.fetchJson("/api/blocks"),
          this.fetchJson("/api/wallet/transactions"),
          this.fetchJson("/api/wallet/utxos"),
          this.fetchJson("/api/mempool"),
          this.fetchJson("/api/peers"),
          this.fetchJson("/api/p2p/metrics"),
        ]);
        this.config = config;
        if (!this.config.setup_complete) {
          await this.refreshWalletSetup();
        }
        this.status = status;
        this.mergeFreshBlocks(blocks, { animateHead: true });
        this.walletTxs = walletTxs;
        this.walletUtxos = walletUtxos;
        this.mempool = mempool;
        this.peers = peers;
        this.p2pMetrics = p2pMetrics;
        this.burnAmount = status.mining?.burn_per_block ?? this.burnAmount;
        this.burnFee = status.mining?.automatic_burn_fee ?? this.burnFee;
        if (!this.burnAmountDirty) {
          this.burnAmountDraft = this.amountLabel(this.burnAmount);
          this.burnFeeDraft = this.amountLabel(this.burnFee);
        }
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
      this.hasMoreBlocks =
        this.blocks.some((block) => block.height > 0) &&
        !this.blocks.some((block) => block.height === 0);

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
      this.$nextTick(() => this.maybeLoadOlderBlocksFromRail());
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

    openTransactionModal(tx, context = {}) {
      this.selectedTransaction = { tx, context };
    },

    closeTransactionModal() {
      this.selectedTransaction = null;
    },

    openWalletUtxosModal() {
      this.showWalletUtxos = true;
    },

    closeWalletUtxosModal() {
      this.showWalletUtxos = false;
    },

    closeModals() {
      this.closeTransactionModal();
      this.closeWalletUtxosModal();
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
        const older = await this.fetchJson(
          `/api/blocks?before_height=${oldest}&limit=${this.blockPageSize}`
        );
        if (
          older.length === 0 ||
          older.length < this.blockPageSize ||
          older.some((block) => block.height === 0)
        ) {
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
      this.maybeLoadOlderBlocksFromRail(event.currentTarget);
    },

    maybeLoadOlderBlocksFromRail(rail = this.$refs.blockRail) {
      if (this.tab !== "chain" || !rail || this.loadingOlder || !this.hasMoreBlocks) return;
      const remaining = rail.scrollWidth - rail.scrollLeft - rail.clientWidth;
      if (remaining <= 180) {
        this.loadOlderBlocks();
      }
    },

    async postForm(path, fields, successMessage) {
      const body = new URLSearchParams();
      for (const [key, value] of Object.entries(fields)) {
        if (Array.isArray(value)) {
          for (const item of value) body.append(key, item);
        } else {
          body.set(key, value);
        }
      }
      const response = await fetch(path, {
        method: "POST",
        headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
        body,
      });
      const text = await response.text();
      let payload = { ok: response.ok, error: null };
      if (text) {
        try {
          payload = JSON.parse(text);
        } catch {
          payload = { ok: false, error: text };
        }
      }
      if (!response.ok || !payload.ok) {
        throw new Error(payload?.error || `${path} returned ${response.status}`);
      }
      await this.refresh();
      this.showFlash(successMessage, "success");
    },

    async saveBurn() {
      try {
        const amount = this.parseLuunAmount(this.burnAmountDraft);
        const fee = this.parseLuunAmount(this.burnFeeDraft);
        this.burnAmountDraft = this.amountLabel(amount);
        this.burnFeeDraft = this.amountLabel(fee);
        await this.postForm(
          "/api/settings/burn-per-block",
          { amount, fee },
          `Burn rate set to ${this.amountLabel(amount)} LUUN per block with ${this.amountLabel(fee)} fee`
        );
        this.burnAmountDirty = false;
        this.burnAmount = amount;
        this.burnFee = fee;
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    automaticBurnFeeDraft() {
      return this.parseLuunAmount(this.burnFeeDraft);
    },

    miningEconomics() {
      return this.status.mining?.economics || {};
    },

    burnSliderMax() {
      return this.amountNumber(this.miningEconomics().slider_max ?? this.latestBlockReward());
    },

    latestBlock() {
      return this.blocks[0] || null;
    },

    latestBlockReward() {
      const latest = this.latestBlock();
      return Math.max(0, Math.trunc(Number(latest?.reward ?? this.status.chain?.block_reward ?? 0)));
    },

    ticketWindow() {
      return Math.max(
        1,
        Math.trunc(Number(this.status.launch_profile?.ticket_expiry_window_heights) || 1)
      );
    },

    estimatedActiveBurnTotal() {
      return Math.max(0, Math.trunc(Number(this.miningEconomics().estimated_active_burn ?? 0)));
    },

    breakEvenBurn() {
      return Math.max(
        0,
        Math.trunc(Number(this.miningEconomics().break_even_burn_microluun) || 0)
      );
    },

    breakEvenPercent() {
      const max = Math.max(0, Math.trunc(Number(this.miningEconomics().slider_max ?? this.latestBlockReward())));
      return max > 0 ? Math.min(100, Math.max(0, (this.breakEvenBurn() / max) * 100)) : 0;
    },

    breakEvenStyle() {
      return `left: ${this.breakEvenPercent()}%`;
    },

    burnBreakEvenLabel() {
      const marker = this.breakEvenBurn();
      const payout = Math.max(0, Math.trunc(Number(this.miningEconomics().last_payout ?? this.latestBlockReward())));
      const burned = this.estimatedActiveBurnTotal();
      const fee = this.status.mining?.automatic_burn_fee ?? this.automaticBurnFeeDraft();
      const window = this.ticketWindow();
      return `Marker: break-even near ${this.amountLabel(marker)} LUUN, using last payout ${this.amountLabel(payout)}, estimated active burns ${this.amountLabel(burned)}, fee ${this.amountLabel(fee)}, and ${window} eligible blocks.`;
    },

    amountLabel(value) {
      const microluun = Math.max(0, Math.trunc(Number(value) || 0));
      const whole = Math.floor(microluun / 1000000);
      const fractional = String(microluun % 1000000).padStart(6, "0").replace(/0+$/, "");
      return fractional ? `${whole}.${fractional}` : `${whole}`;
    },

    amountNumber(value) {
      return Number(this.amountLabel(value));
    },

    parseLuunAmount(value) {
      const text = String(value ?? "").trim();
      if (!text) return 0;
      const match = text.match(/^(\d+)(?:\.(\d{0,6})\d*)?$/);
      if (!match) return 0;
      const whole = Number(match[1] || 0);
      const fractional = Number((match[2] || "").padEnd(6, "0"));
      return Math.max(0, Math.trunc(whole * 1000000 + fractional));
    },

    async sendTransfer() {
      try {
        const amount = this.parseLuunAmount(this.transferAmount);
        const fee = this.parseLuunAmount(this.transferFee);
        const recipient = this.short(this.transferTo);
        await this.postForm(
          "/api/transfer",
          { to: this.transferTo, amount, fee, utxos: this.selectedTransferUtxos.join("\n") },
          `Queued transfer of ${this.amountLabel(amount)} LUUN to ${recipient} with ${this.amountLabel(fee)} fee`
        );
        this.transferTo = "";
        this.transferAmount = null;
        this.selectedTransferUtxos = [];
        this.showSendAdvanced = false;
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    toggleSendAdvanced() {
      this.showSendAdvanced = !this.showSendAdvanced;
      if (!this.showSendAdvanced) {
        this.selectedTransferUtxos = [];
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

    async copyAddress() {
      try {
        await navigator.clipboard.writeText(this.setupAddress());
        this.showFlash("Address copied", "success");
      } catch (error) {
        this.showFlash("Could not copy address", "error");
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

    showSetupFeedback(message, kind) {
      this.setupFeedback = { message, kind };
    },

    short(value) {
      if (!value) return "-";
      if (value.length <= 16) return value;
      return `${value.slice(0, 8)}...${value.slice(-8)}`;
    },

    txFrom(tx) {
      return tx.from ?? tx.inputs?.[0]?.owner ?? "";
    },

    txTo(tx) {
      return tx.to ?? tx.outputs?.[0]?.address ?? null;
    },

    txAmount(tx) {
      return tx.amount ?? tx.outputs?.[0]?.amount ?? 0;
    },

    txInputs(tx) {
      return Array.isArray(tx.inputs) ? tx.inputs : [];
    },

    txVisualOutputs(tx) {
      const rows = [];
      if (tx.kind === "burn" && Number(tx.amount || 0) > 0) {
        rows.push({
          kind: "burned",
          label: "Burn",
          amount: tx.amount,
          address: null,
        });
      }
      if (Number(tx.fee || 0) > 0) {
        rows.push({
          kind: "fee",
          label: "Fee",
          amount: tx.fee,
          address: null,
          detailLabel: "To",
          detail: this.txFeeRecipient(tx),
        });
      }
      const directOutputs = Array.isArray(tx.outputs) ? tx.outputs : [];
      for (const [index, output] of directOutputs.entries()) {
        rows.push({
          kind: "output",
          label: `Output ${index + 1}`,
          amount: output.amount,
          address: output.address,
        });
      }
      const changeOutputs = Array.isArray(tx.change) ? tx.change : [];
      for (const [index, output] of changeOutputs.entries()) {
        rows.push({
          kind: "change",
          label: `Change ${index + 1}`,
          amount: output.amount,
          address: output.address,
        });
      }
      return rows;
    },

    txInputKey(input, index) {
      return `${input.outpoint?.txid || "input"}:${input.outpoint?.index ?? index}`;
    },

    txOutputKey(output, index) {
      return `${output.kind}:${output.address || output.kind}:${output.amount}:${index}`;
    },

    txInputOutpoint(input) {
      const txid = input.outpoint?.txid || "-";
      const index = input.outpoint?.index ?? "-";
      return `${txid}:${index}`;
    },

    utxoOutpoint(utxo) {
      return this.txInputOutpoint({ outpoint: utxo.outpoint });
    },

    selectedTransferUtxoTotal() {
      const selected = new Set(this.selectedTransferUtxos);
      return this.walletUtxos
        .filter((utxo) => selected.has(this.utxoOutpoint(utxo)))
        .reduce((sum, utxo) => sum + Number(utxo.amount || 0), 0);
    },

    transferRequiredTotal() {
      return this.parseLuunAmount(this.transferAmount) + this.parseLuunAmount(this.transferFee);
    },

    selectedTransferUtxosCoverTransfer() {
      return this.selectedTransferUtxos.length === 0 || this.selectedTransferUtxoTotal() >= this.transferRequiredTotal();
    },

    txInputAmountLabel(input) {
      return input.amount === null || input.amount === undefined ? "-" : `LUUN ${this.amountLabel(input.amount)}`;
    },

    txFeeRecipient(tx) {
      const context = this.selectedTransaction?.context || {};
      return tx.blockMiner ?? context.blockMiner ?? "future block miner";
    },

    selectedTransactionLabel() {
      if (!this.selectedTransaction) return "-";
      const { tx, context } = this.selectedTransaction;
      if (context.blockHeight !== undefined) return `Block ${context.blockHeight}`;
      if (tx?.status === "pending") return "Wallet pending";
      if (tx?.blockHeight !== null && tx?.blockHeight !== undefined) {
        return `Wallet block ${tx.blockHeight}`;
      }
      return context.source || "-";
    },

    blockBurned(block) {
      return block.transactions
        .filter((tx) => tx.kind === "burn")
        .reduce((sum, tx) => sum + this.txAmount(tx), 0);
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

    blockMinerLabel(block) {
      const miner = this.short(block.miner);
      return block.miner === this.status.wallet_address ? `${miner} (me)` : miner;
    },

    walletTransactions() {
      return this.walletTxs;
    },

    txTitle(tx) {
      if (tx.status === "pending") return "Pending";
      return tx.blockHeight === null ? "Confirmed" : `Block ${tx.blockHeight}`;
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

    lastUpdatedLabel() {
      return this.lastUpdated ? `Updated ${this.lastUpdated.toLocaleTimeString()}` : "Loading";
    },
  };
};
