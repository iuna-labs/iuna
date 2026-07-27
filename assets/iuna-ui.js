window.iunaApp = function iunaApp() {
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
    auth: { configured: false, authenticated: false },
    authLoaded: false,
    authPassword: "",
    authPasswordConfirm: "",
    loginPassword: "",
    authFeedback: null,
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
    burnFee: 1,
    burnFeeDraft: "0.000001",
    miningEnabled: false,
    powMiningEnabled: false,
    powMineFee: 1,
    powMineFeeDraft: "0.000001",
    powMineFeeDirty: false,
    burnAmountDirty: false,
    transferTo: "",
    transferAmount: null,
    transferFee: "0.000001",
    feeEstimates: { transfer: null, burn: null, mine: null },
    feeEstimateTimer: null,
    showSendAdvanced: false,
    selectedTransferUtxos: [],
    peerAddress: "",
    flash: null,
    flashTimer: null,
    showWalletUtxos: false,
    showPowDifficultyInfo: false,
    lastUpdated: null,
    pollHandle: null,
    hashListenerInstalled: false,
    newBlockHashes: new Set(),
    newBlockTimer: null,
    blockPageSize: 20,

    init() {
      this.bootstrap();
    },

    async bootstrap() {
      await this.refreshAuth();
      if (this.showingAuth()) return;
      await this.bootstrapAuthenticated();
    },

    async bootstrapAuthenticated() {
      await this.refreshConfig();
      if (!this.config.setup_complete) {
        await this.refreshWalletSetup();
      }
      this.tab = this.tabFromHash();
      if (!this.hashListenerInstalled) {
        window.addEventListener("hashchange", () => {
          this.tab = this.tabFromHash();
        });
        this.hashListenerInstalled = true;
      }
      await this.refresh();
      if (!this.pollHandle) {
        this.pollHandle = setInterval(() => this.refresh(), 5000);
      }
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
        wallet: "iuna",
        mining: "Mining",
        p2p: "P2P",
        chain: "Chain",
      }[this.tab] || "iuna";
    },

    showingSetup() {
      return this.authLoaded && !this.showingAuth() && !this.config.setup_complete;
    },

    showingAuth() {
      return this.authLoaded && (!this.auth.configured || !this.auth.authenticated);
    },

    async refreshAuth() {
      this.auth = await this.fetchJson("/api/auth/status");
      this.authLoaded = true;
    },

    async setupPassword() {
      try {
        this.authFeedback = null;
        if (this.authPassword !== this.authPasswordConfirm) {
          throw new Error("Passwords do not match");
        }
        await this.postAuth("/api/auth/setup", this.authPassword);
        this.authPassword = "";
        this.authPasswordConfirm = "";
        await this.refreshAuth();
        await this.bootstrapAuthenticated();
        this.showFlash("Password set", "success");
      } catch (error) {
        this.showAuthFeedback(error.message, "error");
      }
    },

    async login() {
      try {
        this.authFeedback = null;
        await this.postAuth("/api/auth/login", this.loginPassword);
        this.loginPassword = "";
        await this.refreshAuth();
        await this.bootstrapAuthenticated();
        this.showFlash("Logged in", "success");
      } catch (error) {
        this.showAuthFeedback(error.message, "error");
      }
    },

    async postAuth(path, password) {
      const response = await fetch(path, {
        method: "POST",
        headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ password }),
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
        throw new Error(payload.error || `${path} returned ${response.status}`);
      }
      return payload;
    },

    async logout() {
      try {
        await this.postAuth("/api/auth/logout", "");
        await this.refreshAuth();
        this.showFlash("Locked", "success");
      } catch (error) {
        this.showFlash(error.message, "error");
      }
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
          this.fetchJson("/api/blocks?limit=30"),
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
        this.miningEnabled = status.mining?.automatic ?? this.miningEnabled;
        this.powMiningEnabled = status.mining?.pow_mining_enabled ?? this.powMiningEnabled;
        this.powMineFee = status.mining?.automatic_pow_mine_fee ?? this.powMineFee;
        if (!this.burnAmountDirty) {
          this.burnAmountDraft = this.amountLabel(this.burnAmount);
          this.burnFeeDraft = this.amountLabel(this.burnFee);
        }
        if (!this.powMineFeeDirty) {
          this.powMineFeeDraft = this.amountLabel(this.powMineFee);
        }
        this.lastUpdated = new Date();
        this.scheduleFeeEstimates();
      } catch (error) {
        if (String(error.message || "").includes("401")) {
          await this.refreshAuth();
          return;
        }
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

    openPowDifficultyInfo() {
      this.showPowDifficultyInfo = true;
    },

    closePowDifficultyInfo() {
      this.showPowDifficultyInfo = false;
    },

    closeModals() {
      this.closeTransactionModal();
      this.closeWalletUtxosModal();
      this.closePowDifficultyInfo();
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

    async postForm(path, fields, successMessage, method = "POST") {
      const body = new URLSearchParams();
      for (const [key, value] of Object.entries(fields)) {
        if (Array.isArray(value)) {
          for (const item of value) body.append(key, item);
        } else {
          body.set(key, value);
        }
      }
      const response = await fetch(path, {
        method,
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

    scheduleFeeEstimates() {
      if (this.feeEstimateTimer) clearTimeout(this.feeEstimateTimer);
      this.feeEstimateTimer = setTimeout(() => this.refreshFeeEstimates(), 220);
    },

    async refreshFeeEstimates() {
      if (this.showingAuth()) return;
      await Promise.all([
        this.refreshBurnFeeEstimate(),
        this.refreshMineFeeEstimate(),
        this.refreshTransferFeeEstimate(),
      ]);
    },

    async refreshBurnFeeEstimate() {
      const amount = this.parseiunaAmount(this.burnAmountDraft);
      const feePerByte = this.parseiunaAmount(this.burnFeeDraft);
      if (amount <= 0) {
        this.feeEstimates.burn = null;
        return;
      }
      this.feeEstimates.burn = await this.fetchFeeEstimate("/api/fee-estimate/burn", {
        amount,
        fee_per_byte: feePerByte,
      });
    },

    async refreshMineFeeEstimate() {
      const feePerByte = this.parseiunaAmount(this.powMineFeeDraft);
      this.feeEstimates.mine = await this.fetchFeeEstimate("/api/fee-estimate/mine", {
        enabled: this.powMiningEnabled,
        fee_per_byte: feePerByte,
      });
    },

    async refreshTransferFeeEstimate() {
      const amount = this.parseiunaAmount(this.transferAmount);
      const feePerByte = this.parseiunaAmount(this.transferFee);
      if (!this.transferTo.trim() || amount <= 0) {
        this.feeEstimates.transfer = null;
        return;
      }
      this.feeEstimates.transfer = await this.fetchFeeEstimate("/api/fee-estimate/transfer", {
        to: this.transferTo,
        amount,
        fee_per_byte: feePerByte,
        utxos: this.selectedTransferUtxos.join("\n"),
      });
    },

    async fetchFeeEstimate(path, fields) {
      try {
        const body = new URLSearchParams();
        for (const [key, value] of Object.entries(fields)) body.set(key, value);
        const response = await fetch(path, {
          method: "POST",
          headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
          body,
        });
        const payload = await response.json();
        if (!response.ok || !payload.ok) {
          return { error: payload.error || `${path} returned ${response.status}` };
        }
        return payload;
      } catch (error) {
        return { error: error.message };
      }
    },

    feeEstimateLabel(kind) {
      const estimate = this.feeEstimates[kind];
      if (!estimate) return "Enter details to estimate fee";
      if (estimate.error) return estimate.error;
      return `${estimate.bytes} bytes -> IUNA ${this.amountLabel(estimate.fee)}`;
    },

    async saveBurn() {
      try {
        const amount = this.parseiunaAmount(this.burnAmountDraft);
        const fee = this.parseiunaAmountRequired(this.burnFeeDraft, "Burn fee per byte is required");
        this.burnAmountDraft = this.amountLabel(amount);
        this.burnFeeDraft = this.amountLabel(fee);
        await this.postForm(
          "/api/settings/burn-per-block",
          { enabled: this.miningEnabled, amount, fee_per_byte: fee },
          this.miningEnabled
            ? `Finalization burns on: ${this.amountLabel(amount)} IUNA per block with ${this.amountLabel(fee)} per byte`
            : `Burn settings saved while off`
        );
        this.burnAmountDirty = false;
        this.burnAmount = amount;
        this.burnFee = fee;
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async setMiningEnabled(enabled) {
      const previous = this.miningEnabled;
      try {
        const amount = this.parseiunaAmount(this.burnAmountDraft);
        const fee = this.parseiunaAmountRequired(this.burnFeeDraft, "Burn fee per byte is required");
        if (enabled && amount === 0) {
          this.miningEnabled = false;
          throw new Error("Set IUNA per block before turning finalization burns on");
        }
        this.miningEnabled = enabled;
        await this.postForm(
          "/api/settings/burn-per-block",
          { enabled, amount, fee_per_byte: fee },
          enabled ? "Finalization burns turned on" : "Finalization burns turned off"
        );
        this.burnAmountDirty = false;
        this.burnAmount = amount;
        this.burnFee = fee;
      } catch (error) {
        this.miningEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setPowMiningEnabled(enabled) {
      const previous = this.powMiningEnabled;
      try {
        const fee = this.parseiunaAmountRequired(this.powMineFeeDraft, "Mine fee per byte is required");
        this.powMiningEnabled = enabled;
        await this.postForm(
          "/api/settings/pow-mining",
          { enabled, fee_per_byte: fee },
          enabled ? "PoW mining turned on" : "PoW mining turned off"
        );
        this.powMineFeeDirty = false;
        this.powMineFee = fee;
      } catch (error) {
        this.powMiningEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    async savePowMining() {
      try {
        const fee = this.parseiunaAmountRequired(this.powMineFeeDraft, "Mine fee per byte is required");
        this.powMineFeeDraft = this.amountLabel(fee);
        await this.postForm(
          "/api/settings/pow-mining",
          { enabled: this.powMiningEnabled, fee_per_byte: fee },
          this.powMiningEnabled
            ? `Mine fee rate set to ${this.amountLabel(fee)} IUNA per byte`
            : `Mine settings saved while off`
        );
        this.powMineFeeDirty = false;
        this.powMineFee = fee;
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    automaticBurnFeeDraft() {
      return this.parseiunaAmount(this.burnFeeDraft);
    },

    powMineFeeValue() {
      try {
        return this.parseiunaAmount(this.powMineFeeDraft);
      } catch {
        return this.powMineFee;
      }
    },

    powMineNetReward() {
      const reward = Math.max(0, Math.trunc(Number(this.status.chain?.mine_reward ?? 0)));
      return Math.max(0, reward - (this.feeEstimates.mine?.fee ?? this.powMineFeeValue()));
    },

    amountLabel(value) {
      const microiuna = Math.max(0, Math.trunc(Number(value) || 0));
      const whole = Math.floor(microiuna / 1000000);
      const fractional = String(microiuna % 1000000).padStart(6, "0").replace(/0+$/, "");
      return fractional ? `${whole}.${fractional}` : `${whole}`;
    },

    amountNumber(value) {
      return Number(this.amountLabel(value));
    },

    parseiunaAmount(value) {
      const text = String(value ?? "").trim();
      if (!text) return 0;
      const match = text.match(/^(\d+)(?:\.(\d{0,6})\d*)?$/);
      if (!match) return 0;
      const whole = Number(match[1] || 0);
      const fractional = Number((match[2] || "").padEnd(6, "0"));
      return Math.max(0, Math.trunc(whole * 1000000 + fractional));
    },

    parseiunaAmountRequired(value, message) {
      const text = String(value ?? "").trim();
      if (!text) throw new Error(message);
      const parsed = this.parseiunaAmount(text);
      if (parsed === 0 && !/^0(?:\.0*)?$/.test(text)) throw new Error(message);
      return parsed;
    },

    async sendTransfer() {
      try {
        const amount = this.parseiunaAmount(this.transferAmount);
        const fee = this.parseiunaAmountRequired(this.transferFee, "Transfer fee per byte is required");
        const recipient = this.short(this.transferTo);
        await this.postForm(
          "/api/transfer",
          { to: this.transferTo, amount, fee_per_byte: fee, utxos: this.selectedTransferUtxos.join("\n") },
          `Queued transfer of ${this.amountLabel(amount)} IUNA to ${recipient}`
        );
        this.transferTo = "";
        this.transferAmount = null;
        this.selectedTransferUtxos = [];
        this.showSendAdvanced = false;
        this.feeEstimates.transfer = null;
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
        const peer = this.peerAddress.trim();
        await this.postForm("/api/peers", { peer }, `Added peer ${peer}`);
        this.peerAddress = "";
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async removePeer(peer) {
      try {
        await this.postForm("/api/peers", { peer: peer.address }, `Removed peer ${peer.address}`, "DELETE");
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

    showAuthFeedback(message, kind) {
      this.authFeedback = { message, kind };
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

    isMineTx(tx) {
      return tx?.kind === "mine";
    },

    txDifficultyBits(tx) {
      return tx?.difficulty_bits ?? tx?.difficultyBits ?? null;
    },

    txProofBits(tx) {
      return tx?.proof_bits ?? tx?.proofBits ?? null;
    },

    txProofHash(tx) {
      return tx?.proof_hash ?? tx?.proofHash ?? tx?.signature ?? null;
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

    selectAllTransferUtxos() {
      this.selectedTransferUtxos = this.walletUtxos.map((utxo) => this.utxoOutpoint(utxo));
      this.scheduleFeeEstimates();
    },

    clearTransferUtxos() {
      this.selectedTransferUtxos = [];
      this.scheduleFeeEstimates();
    },

    selectedTransferUtxoTotal() {
      const selected = new Set(this.selectedTransferUtxos);
      return this.walletUtxos
        .filter((utxo) => selected.has(this.utxoOutpoint(utxo)))
        .reduce((sum, utxo) => sum + Number(utxo.amount || 0), 0);
    },

    transferRequiredTotal() {
      return this.parseiunaAmount(this.transferAmount) + Number(this.feeEstimates.transfer?.fee || 0);
    },

    selectedTransferUtxosCoverTransfer() {
      return this.selectedTransferUtxos.length === 0 || this.selectedTransferUtxoTotal() >= this.transferRequiredTotal();
    },

    txInputAmountLabel(input) {
      return input.amount === null || input.amount === undefined ? "-" : `IUNA ${this.amountLabel(input.amount)}`;
    },

    txFeeRecipient(tx) {
      const context = this.selectedTransaction?.context || {};
      return tx.blockFinalizer ?? tx.blockMiner ?? context.blockFinalizer ?? context.blockMiner ?? "future block finalizer";
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

    blockTotalFees(block) {
      const explicitTotal = block?.totalFees ?? block?.total_fees ?? block?.reward;
      if (explicitTotal !== null && explicitTotal !== undefined) return Number(explicitTotal) || 0;
      return (block?.transactions || []).reduce((sum, tx) => sum + Number(tx.fee || 0), 0);
    },

    recentBlockFeeAverage(count) {
      const sample = this.blocks.filter((block) => block.height > 0).slice(0, count);
      if (sample.length === 0) return 0;
      return Math.round(sample.reduce((sum, block) => sum + this.blockTotalFees(block), 0) / sample.length);
    },

    blockBurnCount(block) {
      return block.transactions.filter((tx) => tx.kind === "burn").length;
    },

    blockTransferCount(block) {
      return block.transactions.filter((tx) => tx.kind === "transfer").length;
    },

    blockMineCount(block) {
      return block.transactions.filter((tx) => tx.kind === "mine").length;
    },

    burnCountLabel(block) {
      const count = this.blockBurnCount(block);
      return `${count} burn${count === 1 ? "" : "s"}`;
    },

    transferCountLabel(block) {
      const count = this.blockTransferCount(block);
      return `${count} transfer${count === 1 ? "" : "s"}`;
    },

    mineCountLabel(block) {
      const count = this.blockMineCount(block);
      return `${count} mine${count === 1 ? "" : "s"}`;
    },

    blockFinalizerLabel(block) {
      const finalizer = this.short(block.miner);
      return block.miner === this.status.wallet_address ? `${finalizer} (me)` : finalizer;
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

    outboundPeers() {
      return this.peers.filter((peer) => peer.direction !== "inbound");
    },

    inboundPeers() {
      return this.peers.filter((peer) => peer.direction === "inbound");
    },

    healthyPeers() {
      return this.peers.filter((peer) => !peer.last_error && typeof peer.last_known_height === "number");
    },

    failedPeers() {
      return this.peers.filter((peer) => peer.last_error);
    },

    peerStatus(peer) {
      if (peer.last_error) return "error";
      if (typeof peer.last_known_height === "number") return "synced";
      if ((peer.messages_sent ?? 0) > 0 || (peer.messages_received ?? 0) > 0) return "active";
      return "pending";
    },

    peerStatusLabel(peer) {
      return {
        error: "Error",
        synced: "Synced",
        active: "Active",
        pending: "Pending",
      }[this.peerStatus(peer)];
    },

    peerHeightDelta(peer) {
      const local = this.status.chain?.height;
      const remote = peer.last_known_height;
      if (typeof local !== "number" || typeof remote !== "number") return "-";
      if (remote === local) return "even";
      if (remote > local) return `+${remote - local}`;
      return `-${local - remote}`;
    },

    canRemovePeer(peer) {
      return peer.direction !== "inbound";
    },

    targetSecondsLabel() {
      const ms = this.status.mining?.vdf_target_block_ms;
      if (!ms) return "-";
      const seconds = Math.round(ms / 1000);
      if (seconds % 60 === 0) return `${seconds / 60}m`;
      return `${seconds}s`;
    },

    stratumListenAddr() {
      return this.status.stratum?.listen_addr || "-";
    },

    stratumPoolUrl() {
      const listen = this.status.stratum?.listen_addr;
      if (!this.status.stratum?.enabled || !listen) return "-";
      const lastColon = listen.lastIndexOf(":");
      if (lastColon < 0) return `stratum+tcp://${listen}`;
      let host = listen.slice(0, lastColon);
      const port = listen.slice(lastColon + 1);
      if (host === "0.0.0.0" || host === "::" || host === "[::]") {
        host = window.location.hostname || "127.0.0.1";
      }
      return `stratum+tcp://${host}:${port}`;
    },

    lastUpdatedLabel() {
      return this.lastUpdated ? `Updated ${this.lastUpdated.toLocaleTimeString()}` : "Loading";
    },
  };
};
