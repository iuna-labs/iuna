window.iunaApp = function iunaApp() {
  return {
    tab: "wallet",
    status: {},
    blocks: [],
    selectedBlock: null,
    selectedTransaction: null,
    selectedBurnLeaderBlock: null,
    loadingOlder: false,
    hasMoreBlocks: true,
    walletTxs: [],
    walletUtxos: [],
    mempool: [],
    peers: [],
    p2pMetrics: {},
    blockchainMetrics: { enabled: false, latest: null, charts: [] },
    metricHover: null,
    metricsRange: (() => {
      try {
        const stored = localStorage.getItem("iunaMetricsRange");
        if (stored === "1000") return 1000;
        if (stored === "all") return "all";
      } catch {
        // Ignore storage failures; the in-memory default is enough.
      }
      return 100;
    })(),
    networkHealth: {},
    uiMode: (() => {
      try {
        return localStorage.getItem("iunaUiMode") === "advanced" ? "advanced" : "basic";
      } catch {
        return "basic";
      }
    })(),
    latestRelease: null,
    releaseCheckState: "idle",
    releaseCheckError: null,
    config: { setup_complete: false },
    auth: { configured: false, authenticated: false },
    authLoaded: false,
    authPassword: "",
    authPasswordConfirm: "",
    loginPassword: "",
    authFeedback: null,
    settingsOldPassword: "",
    settingsNewPassword: "",
    settingsPasswordConfirm: "",
    settingsFeedback: null,
    keepTrackOfMetrics: false,
    p2pAcceptInbound: false,
    p2pAnnounceAddr: "",
    p2pAnnounceDirty: false,
    setupWallet: { address: null, seed_phrase: null, dev_verify_bypass: false, requires_peer: false },
    setupNodeMode: "wallet",
    setupWalletMode: "create",
    setupSeedStep: "write",
    generatedSeedPhrase: "",
    verifyChallenges: [],
    verifyAnswers: {},
    importSeedPhrase: "",
    walletVerified: false,
    setupFeedback: null,
    burnAmount: 100,
    burnAmountDraft: "0.0001",
    burnFee: 100,
    burnFeeDraft: "0.0001",
    miningEnabled: false,
    powMiningEnabled: false,
    burnAmountDirty: false,
    transferTo: "",
    transferAmount: null,
    transferFee: "0.000001",
    feeEstimates: { transfer: null, burn: null, mine: null },
    feeEstimateTimer: null,
    showSendAdvanced: false,
    selectedTransferUtxos: [],
    selectedTransferUtxoAmounts: {},
    walletTxFilters: { transfer: true, mine: false, burn: false },
    setupPeerAddress: "iuna.jhx.app:9444",
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
    datasetPageSize: 25,
    walletTxPage: { offset: 0, total: 0, hasMore: true, loading: false, backgroundLoading: false },
    walletUtxoPage: { offset: 0, total: 0, hasMore: true, loading: false, backgroundLoading: false },
    mempoolPage: { offset: 0, total: 0, hasMore: true, loading: false, backgroundLoading: false },
    peerPage: { offset: 0, total: 0, hasMore: true, loading: false, backgroundLoading: false },

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
      this.checkLatestRelease();
      if (!this.pollHandle) {
        this.pollHandle = setInterval(() => this.refresh({ silent: true }), 5000);
      }
    },

    tabFromHash() {
      const hash = window.location.hash.replace(/^#\/?/, "");
      return this.allowedTabs().includes(hash) ? hash : "wallet";
    },

    setTab(tab) {
      if (!this.allowedTabs().includes(tab)) return;
      this.tab = tab;
      if (window.location.hash !== `#${tab}`) {
        window.location.hash = tab;
      }
    },

    allowedTabs() {
      const tabs = this.advancedMode()
        ? ["wallet", "mining", "p2p", "chain", "settings"]
        : ["wallet", "chain", "settings"];
      if (this.config.keep_track_of_metrics) {
        tabs.splice(tabs.indexOf("chain") + 1, 0, "metrics");
      }
      return tabs;
    },

    basicMode() {
      return this.uiMode !== "advanced";
    },

    advancedMode() {
      return this.uiMode === "advanced";
    },

    setUiMode(mode) {
      this.uiMode = mode === "advanced" ? "advanced" : "basic";
      try {
        localStorage.setItem("iunaUiMode", this.uiMode);
      } catch {}
      if (!this.allowedTabs().includes(this.tab)) {
        this.setTab("wallet");
      }
    },

    toggleUiMode() {
      this.setUiMode(this.advancedMode() ? "basic" : "advanced");
    },

    pageTitle() {
      return {
        wallet: "iuna",
        mining: "Mining",
        p2p: "P2P",
        chain: "Chain",
        metrics: "Metrics",
        settings: "Settings",
      }[this.tab] || "iuna";
    },

    appVersionLabel() {
      return `v${this.normalizeVersion(this.status.app_version || "0.0.0")}`;
    },

    latestReleaseLabel() {
      return this.latestRelease?.tag || "";
    },

    updateAvailable() {
      const current = this.status.app_version;
      const latest = this.latestRelease?.tag;
      if (!current || !latest) return false;
      return this.compareVersions(latest, current) > 0;
    },

    versionPanelTitle() {
      if (this.updateAvailable()) return `Update available: ${this.latestReleaseLabel()}`;
      if (this.releaseCheckState === "failed") return this.releaseCheckError || "Could not check latest release";
      if (this.releaseCheckState === "checking") return "Checking latest release";
      return "iuna is up to date";
    },

    openLatestRelease() {
      const url = this.latestRelease?.url || "https://github.com/iuna-labs/iuna/releases";
      window.open(url, "_blank", "noopener,noreferrer");
    },

    showingSetup() {
      return this.authLoaded && !this.showingAuth() && !this.config.setup_complete;
    },

    showingAuth() {
      return this.authLoaded && (!this.auth.configured || !this.auth.authenticated);
    },

    setupRequiresPeer() {
      return this.setupWallet.requires_peer === true;
    },

    setupHasPeer() {
      return this.setupPeerAddress.trim().length > 0 || this.outboundPeers().length > 0;
    },

    setupCanContinue() {
      return this.walletVerified && (!this.setupRequiresPeer() || this.setupHasPeer());
    },

    selectSetupNodeMode(mode) {
      this.setupNodeMode = ["wallet", "non-listening", "listening"].includes(mode)
        ? mode
        : "wallet";
      this.setupFeedback = null;
    },

    setupNodeModeCopy() {
      if (this.setupNodeMode === "listening") {
        return "Listening node shows mining and P2P controls and accepts inbound P2P connections when TCP port 9444 is reachable.";
      }
      if (this.setupNodeMode === "non-listening") {
        return "Non-listening node shows mining and P2P controls, connects out to peers, and keeps inbound P2P closed.";
      }
      return "Wallet mode keeps the interface focused on your wallet and chain, while this node only connects out to peers.";
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

    async changePassword() {
      try {
        this.settingsFeedback = null;
        if (this.settingsNewPassword !== this.settingsPasswordConfirm) {
          throw new Error("New passwords do not match");
        }
        const body = new URLSearchParams({
          old_password: this.settingsOldPassword,
          new_password: this.settingsNewPassword,
        });
        const response = await fetch("/api/auth/change-password", {
          method: "POST",
          headers: {
            Accept: "application/json",
            "Content-Type": "application/x-www-form-urlencoded",
          },
          body,
        });
        const payload = await response.json();
        if (!response.ok || !payload.ok) {
          throw new Error(payload.error || `/api/auth/change-password returned ${response.status}`);
        }
        this.settingsOldPassword = "";
        this.settingsNewPassword = "";
        this.settingsPasswordConfirm = "";
        await this.refreshAuth();
        this.showSettingsFeedback("Password changed", "success");
        this.showFlash("Password changed", "success");
      } catch (error) {
        this.showSettingsFeedback(error.message, "error");
      }
    },

    async refreshConfig() {
      this.config = await this.fetchJson("/api/config");
      this.syncConfigState();
    },

    syncConfigState() {
      this.keepTrackOfMetrics = this.config.keep_track_of_metrics === true;
      this.p2pAcceptInbound = this.config.p2p_accept_inbound === true;
      if (!this.p2pAnnounceDirty) {
        this.p2pAnnounceAddr = this.config.p2p_announce_addr || "";
      }
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
        if (this.setupRequiresPeer() && !this.setupHasPeer()) {
          throw new Error("Add a bootstrap peer before continuing");
        }
        await this.applySetupNodeMode();
        const response = await fetch("/api/config", {
          method: "POST",
          headers: {
            Accept: "application/json",
            "Content-Type": "application/x-www-form-urlencoded",
          },
          body: new URLSearchParams({
            setup_complete: "true",
            peer: this.setupPeerAddress.trim(),
          }),
        });
        const payload = await response.json();
        if (!response.ok || !payload.ok) {
          throw new Error(payload.error || `/api/config returned ${response.status}`);
        }
        await this.refresh();
        this.setupFeedback = null;
        this.generatedSeedPhrase = "";
        this.importSeedPhrase = "";
        this.setupPeerAddress = "";
        this.verifyChallenges = [];
        this.verifyAnswers = {};
        this.showFlash("Setup complete", "success");
        this.setTab("wallet");
      } catch (error) {
        this.showSetupFeedback(error.message, "error");
      }
    },

    async applySetupNodeMode() {
      const mode = ["wallet", "non-listening", "listening"].includes(this.setupNodeMode)
        ? this.setupNodeMode
        : "wallet";
      const acceptInbound = mode === "listening";
      if (this.p2pAcceptInbound !== acceptInbound) {
        await this.submitForm("/api/settings/p2p-inbound", { enabled: acceptInbound });
        this.p2pAcceptInbound = acceptInbound;
      }
      this.setUiMode(mode === "wallet" ? "basic" : "advanced");
    },

    async refresh(options = {}) {
      try {
        const [config, status, blocks, p2pMetrics, blockchainMetrics, networkHealth] = await Promise.all([
          this.fetchJson("/api/config"),
          this.fetchJson("/api/status"),
          this.fetchJson("/api/blocks?limit=30"),
          this.fetchJson("/api/p2p/metrics"),
          this.fetchJson("/api/metrics"),
          this.fetchJson("/api/network/health"),
        ]);
        await Promise.all([
          this.refreshPagedDataset("walletTx", { silent: options.silent === true }),
          this.refreshPagedDataset("walletUtxo", { silent: options.silent === true }),
          this.refreshPagedDataset("mempool", { silent: options.silent === true }),
          this.refreshPagedDataset("peer", { silent: options.silent === true }),
        ]);
        this.config = config;
        this.syncConfigState();
        if (!this.allowedTabs().includes(this.tab)) {
          this.setTab("wallet");
        }
        if (!this.config.setup_complete) {
          await this.refreshWalletSetup();
        }
        this.status = status;
        this.mergeFreshBlocks(blocks, { animateHead: true });
        this.pruneSelectedTransferUtxos();
        this.p2pMetrics = p2pMetrics;
        this.blockchainMetrics = blockchainMetrics;
        this.networkHealth = networkHealth;
        this.burnAmount = status.mining?.burn_per_block ?? this.burnAmount;
        this.burnFee = status.mining?.automatic_burn_fee ?? this.burnFee;
        this.miningEnabled = status.mining?.automatic ?? this.miningEnabled;
        this.powMiningEnabled = status.mining?.pow_mining_enabled ?? this.powMiningEnabled;
        if (!this.burnAmountDirty) {
          this.burnAmountDraft = this.amountLabel(this.burnAmount);
          this.burnFeeDraft = this.amountLabel(this.burnFee);
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

    datasetConfig(kind) {
      return {
        walletTx: {
          items: "walletTxs",
          page: "walletTxPage",
          path: () => this.walletTransactionsPath(),
          key: (tx) => `${tx.status || ""}:${tx.signature || ""}`,
        },
        walletUtxo: {
          items: "walletUtxos",
          page: "walletUtxoPage",
          path: () => "/api/wallet/utxos",
          key: (utxo) => this.utxoOutpoint(utxo),
        },
        mempool: {
          items: "mempool",
          page: "mempoolPage",
          path: () => "/api/mempool",
          key: (tx) => tx.signature || "",
        },
        peer: {
          items: "peers",
          page: "peerPage",
          path: () => "/api/peers",
          key: (peer) => peer.address || "",
        },
      }[kind];
    },

    async resetPagedDataset(kind) {
      const config = this.datasetConfig(kind);
      if (!config) return;
      this[config.items] = [];
      Object.assign(this[config.page], {
        offset: 0,
        total: 0,
        hasMore: true,
        loading: false,
        backgroundLoading: false,
      });
      await this.refreshPagedDataset(kind);
    },

    async refreshPagedDataset(kind, options = {}) {
      const config = this.datasetConfig(kind);
      if (!config) return;
      const page = this[config.page];
      if (page.loading || page.backgroundLoading) return;
      const currentLength = this[config.items].length;
      const limit = Math.max(this.datasetPageSize, currentLength || 0);
      await this.loadPagedDataset(kind, {
        offset: 0,
        limit,
        replace: true,
        silent: options.silent === true,
      });
    },

    async loadNextPage(kind) {
      const config = this.datasetConfig(kind);
      if (!config) return;
      const page = this[config.page];
      if (page.loading || page.backgroundLoading || !page.hasMore) return;
      await this.loadPagedDataset(kind, {
        offset: page.offset ?? this[config.items].length,
        limit: this.datasetPageSize,
        replace: false,
      });
    },

    async loadPagedDataset(kind, options) {
      const config = this.datasetConfig(kind);
      const page = this[config.page];
      const loadingKey = options.silent === true ? "backgroundLoading" : "loading";
      page[loadingKey] = true;
      try {
        const payload = await this.fetchJson(
          this.paginatedPath(config.path(), options.offset, options.limit)
        );
        const normalized = this.normalizedPage(payload, options.offset, options.limit);
        this[config.items] = options.replace
          ? normalized.items
          : this.mergeDatasetItems(this[config.items], normalized.items, config.key);
        page.offset = normalized.nextOffset ?? this[config.items].length;
        page.total = normalized.total;
        page.hasMore = normalized.hasMore;
        if (kind === "walletUtxo") {
          this.rememberUtxoAmounts(this.walletUtxos);
          this.pruneSelectedTransferUtxos();
        }
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        page[loadingKey] = false;
      }
    },

    paginatedPath(path, offset, limit) {
      const url = new URL(path, window.location.origin);
      url.searchParams.set("offset", String(offset));
      url.searchParams.set("limit", String(limit));
      return `${url.pathname}?${url.searchParams.toString()}`;
    },

    normalizedPage(payload, offset, limit) {
      if (Array.isArray(payload)) {
        const nextOffset = offset + payload.length;
        return {
          items: payload,
          total: nextOffset,
          hasMore: payload.length >= limit,
          nextOffset,
        };
      }
      const items = Array.isArray(payload?.items) ? payload.items : [];
      return {
        items,
        total: Number(payload?.total ?? offset + items.length),
        hasMore: payload?.hasMore === true,
        nextOffset: payload?.nextOffset ?? offset + items.length,
      };
    },

    mergeDatasetItems(existing, incoming, keyFn) {
      const rows = [];
      const seen = new Set();
      for (const item of [...existing, ...incoming]) {
        const key = keyFn(item);
        if (!key || seen.has(key)) continue;
        seen.add(key);
        rows.push(item);
      }
      return rows;
    },

    observePageSentinel(kind, element) {
      if (!element || element.__iunaPageObserver) return;
      const observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          this.loadNextPage(kind);
        }
      }, { root: null, rootMargin: "180px 0px" });
      observer.observe(element);
      element.__iunaPageObserver = observer;
    },

    observeBlockSentinel(element) {
      if (!element || element.__iunaBlockObserver) return;
      const observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          this.loadOlderBlocks();
        }
      }, { root: null, rootMargin: "180px 0px" });
      observer.observe(element);
      element.__iunaBlockObserver = observer;
    },

    walletTransactionsPath() {
      const params = new URLSearchParams({
        tx: String(this.walletTxFilters.transfer),
        mine: String(this.walletTxFilters.mine),
        burn: String(this.walletTxFilters.burn),
      });
      return `/api/wallet/transactions?${params.toString()}`;
    },

    async refreshWalletTransactions() {
      await this.resetPagedDataset("walletTx");
    },

    async checkLatestRelease() {
      if (this.releaseCheckState === "checking") return;
      this.releaseCheckState = "checking";
      this.releaseCheckError = null;
      try {
        const response = await fetch("https://api.github.com/repos/iuna-labs/iuna/releases/latest", {
          headers: { Accept: "application/vnd.github+json" },
        });
        if (!response.ok) throw new Error(`release check returned ${response.status}`);
        const release = await response.json();
        this.latestRelease = {
          tag: release.tag_name || "",
          url: release.html_url || "https://github.com/iuna-labs/iuna/releases",
        };
        this.releaseCheckState = "done";
      } catch (error) {
        this.releaseCheckError = error.message || "Release check failed";
        this.releaseCheckState = "failed";
      }
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

    openBurnLeaderRanksModal(block) {
      this.selectedBurnLeaderBlock = block;
    },

    closeBurnLeaderRanksModal() {
      this.selectedBurnLeaderBlock = null;
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
      this.closeBurnLeaderRanksModal();
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
      await this.submitForm(path, fields, method);
      await this.refresh();
      this.showFlash(successMessage, "success");
    },

    async submitForm(path, fields, method = "POST") {
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
      return payload;
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
      this.feeEstimates.mine = await this.fetchFeeEstimate("/api/fee-estimate/mine", {});
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
        if (amount === 0) {
          throw new Error("IUNA per block must be greater than zero");
        }
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
        this.powMiningEnabled = enabled;
        await this.postForm(
          "/api/settings/pow-mining",
          { enabled },
          enabled ? "PoW mining turned on" : "PoW mining turned off"
        );
      } catch (error) {
        this.powMiningEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setKeepTrackOfMetrics(enabled) {
      const previous = this.keepTrackOfMetrics;
      try {
        this.keepTrackOfMetrics = enabled;
        await this.postForm(
          "/api/settings/metrics",
          { enabled },
          enabled ? "Metrics tracking turned on" : "Metrics tracking turned off"
        );
        await this.refreshConfig();
        if (!enabled && this.tab === "metrics") {
          this.setTab("settings");
        }
      } catch (error) {
        this.keepTrackOfMetrics = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setP2pAcceptInbound(enabled) {
      const previous = this.p2pAcceptInbound;
      try {
        this.p2pAcceptInbound = enabled;
        await this.postForm(
          "/api/settings/p2p-inbound",
          { enabled },
          enabled ? "Public node enabled" : "Switched to outbound-only P2P"
        );
        await this.refreshConfig();
      } catch (error) {
        this.p2pAcceptInbound = previous;
        this.showFlash(error.message, "error");
      }
    },

    async saveP2pAnnounce() {
      if (!this.p2pAcceptInbound) {
        this.showFlash("Enable public node before setting a public P2P address", "error");
        return;
      }
      const addr = this.p2pAnnounceAddr.trim();
      try {
        await this.postForm(
          "/api/settings/p2p-announce",
          { addr },
          addr ? "P2P announce address saved" : "P2P announce address cleared"
        );
        this.p2pAnnounceAddr = addr;
        this.p2pAnnounceDirty = false;
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    automaticBurnFeeDraft() {
      return this.parseiunaAmount(this.burnFeeDraft);
    },

    powMineReward() {
      return Math.max(0, Math.trunc(Number(this.status.chain?.mine_reward ?? 1000000)));
    },

    autoPowStatusLabel() {
      if (!this.powMiningEnabled) return "PoW mining is off";
      return this.status.mining?.last_auto_pow_mine_status || "Waiting for next automatic PoW mining tick";
    },

    metricsCharts() {
      return Array.isArray(this.blockchainMetrics?.charts) ? this.blockchainMetrics.charts : [];
    },

    metricsLatest() {
      return this.blockchainMetrics?.latest || {};
    },

    setMetricsRange(range) {
      this.metricsRange = range === 1000 || range === "all" ? range : 100;
      this.metricHover = null;
      try {
        localStorage.setItem("iunaMetricsRange", String(this.metricsRange));
      } catch {
        // Non-persistent filtering is fine when storage is unavailable.
      }
    },

    metricChartPoints(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return "";
      const bounds = this.metricChartBounds(chart);
      return points
        .map((point) => {
          const x = this.metricXAxisPositionFromBounds(bounds, Number(point.height));
          const y = this.metricYAxisPositionFromBounds(bounds, Number(point.value));
          return `${x.toFixed(1)},${y.toFixed(1)}`;
        })
        .join(" ");
    },

    metricChartPointMarkers(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return [];
      const bounds = this.metricChartBounds(chart);
      return points.map((point) => {
        const height = Number(point.height);
        const value = Number(point.value);
        return {
          height,
          value,
          x: this.metricXAxisPositionFromBounds(bounds, height),
          y: this.metricYAxisPositionFromBounds(bounds, value),
          label: this.metricPointLabel(chart, point),
        };
      });
    },

    metricGridPath(chart) {
      const yLines = this.metricYAxisTicks(chart).map((tick) => {
        const y = this.metricYAxisPositionFromBounds(this.metricChartBounds(chart), Number(tick)).toFixed(1);
        return `M4 ${y} H296`;
      });
      const xLines = this.metricXAxisTicks(chart).map((tick) => {
        const x = this.metricXAxisPositionFromBounds(this.metricChartBounds(chart), Number(tick)).toFixed(1);
        return `M${x} 8 V132`;
      });
      return [...yLines, ...xLines].join(" ");
    },

    metricVisiblePoints(chart) {
      const points = Array.isArray(chart?.points) ? chart.points : [];
      const validPoints = points.filter((point) => Number.isFinite(Number(point.value)));
      const limit = this.metricsRange;
      if (limit === "all") return validPoints;
      const latestHeight = Number(this.metricsLatest().height);
      if (!Number.isFinite(latestHeight)) return validPoints.slice(-limit);
      const minHeight = Math.max(0, latestHeight - limit + 1);
      return validPoints.filter((point) => Number(point.height) >= minHeight);
    },

    metricLatestValueLabel(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return "-";
      return this.metricValueLabel(chart, points[points.length - 1].value);
    },

    metricChartBounds(chart) {
      const points = this.metricVisiblePoints(chart);
      const heights = points.map((point) => Number(point.height));
      const values = points.map((point) => Number(point.value));
      const valueTicks = this.niceTicks(Math.min(...values), Math.max(...values), 5);
      return {
        minHeight: Math.min(...heights),
        maxHeight: Math.max(...heights),
        minValue: Math.min(...valueTicks),
        maxValue: Math.max(...valueTicks),
      };
    },

    metricYAxisTicks(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return [];
      const values = points.map((point) => Number(point.value));
      return this.niceTicks(Math.min(...values), Math.max(...values), 5).reverse();
    },

    metricXAxisTicks(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return [];
      const heights = points.map((point) => Number(point.height));
      const minHeight = Math.min(...heights);
      const maxHeight = Math.max(...heights);
      if (minHeight === maxHeight) return [minHeight];
      return this.niceTicks(minHeight, maxHeight, 5)
        .map((tick) => Math.round(tick))
        .filter((tick) => tick >= minHeight && tick <= maxHeight)
        .filter((tick, index, ticks) => ticks.indexOf(tick) === index);
    },

    niceTicks(minValue, maxValue, maxTicks = 5) {
      const min = Number(minValue);
      const max = Number(maxValue);
      if (!Number.isFinite(min) || !Number.isFinite(max)) return [];
      if (min === max) {
        if (min === 0) return [0];
        const step = this.niceTickStep(Math.abs(min) / Math.max(1, maxTicks - 1));
        const tickMin = Math.floor(Math.min(0, min) / step) * step;
        const tickMax = Math.ceil(max / step) * step;
        return this.tickRange(tickMin, tickMax, step);
      }
      const range = this.niceTickStep((max - min) / Math.max(1, maxTicks - 1));
      const tickMin = Math.floor(min / range) * range;
      const tickMax = Math.ceil(max / range) * range;
      return this.tickRange(tickMin, tickMax, range);
    },

    niceTickStep(value) {
      if (!Number.isFinite(value) || value <= 0) return 1;
      const exponent = Math.floor(Math.log10(value));
      const fraction = value / Math.pow(10, exponent);
      const niceFraction = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 5 ? 5 : 10;
      return niceFraction * Math.pow(10, exponent);
    },

    tickRange(min, max, step) {
      if (!Number.isFinite(step) || step <= 0) return [];
      const precision = Math.max(0, Math.ceil(-Math.log10(step)) + 2);
      const ticks = [];
      for (let tick = min; tick <= max + step / 2; tick += step) {
        ticks.push(Number(tick.toFixed(precision)));
        if (ticks.length > 8) break;
      }
      return ticks;
    },

    metricYAxisPositionFromBounds(bounds, value) {
      const valueRange = Math.max(1, bounds.maxValue - bounds.minValue);
      return 132 - ((value - bounds.minValue) / valueRange) * 124;
    },

    metricXAxisPositionFromBounds(bounds, height) {
      const heightRange = Math.max(1, bounds.maxHeight - bounds.minHeight);
      return 4 + ((height - bounds.minHeight) / heightRange) * 292;
    },

    metricYAxisLabelStyle(chart, value) {
      const y = this.metricYAxisPositionFromBounds(this.metricChartBounds(chart), Number(value));
      return `top: ${(y / 148) * 100}%`;
    },

    metricXAxisLabelStyle(chart, height) {
      const x = this.metricXAxisPositionFromBounds(this.metricChartBounds(chart), Number(height));
      return `left: ${(x / 300) * 100}%`;
    },

    metricPointStyle(marker) {
      return `left: ${(marker.x / 300) * 100}%; top: ${(marker.y / 148) * 100}%;`;
    },

    setMetricHover(chart, marker) {
      this.metricHover = {
        chartId: chart.id,
        height: marker.height,
        value: marker.value,
        x: marker.x,
        y: marker.y,
        label: marker.label,
      };
    },

    setMetricHoverFromPlot(chart, event) {
      const markers = this.metricChartPointMarkers(chart);
      if (markers.length === 0) {
        this.clearMetricHover(chart);
        return;
      }
      const rect = event.currentTarget.getBoundingClientRect();
      const relativeX = Math.min(Math.max(event.clientX - rect.left, 0), rect.width);
      const chartX = (relativeX / Math.max(1, rect.width)) * 300;
      const nearest = markers.reduce((best, marker) => {
        const distance = Math.abs(marker.x - chartX);
        return !best || distance < best.distance ? { marker, distance } : best;
      }, null)?.marker;
      if (nearest) {
        this.setMetricHover(chart, nearest);
      }
    },

    clearMetricHover(chart) {
      if (this.metricHover?.chartId === chart.id) {
        this.metricHover = null;
      }
    },

    metricTooltipLabel(chart) {
      return this.metricHover?.chartId === chart.id ? this.metricHover.label : "";
    },

    metricTooltipStyle(chart) {
      const hover = this.metricHover;
      if (!hover || hover.chartId !== chart.id) return "";
      const left = (hover.x / 300) * 100;
      const top = (hover.y / 148) * 100;
      const xShift = hover.x > 238 ? "-100%" : hover.x < 62 ? "0" : "-50%";
      const yShift = hover.y < 34 ? "12px" : "-115%";
      return `left: ${left}%; top: ${top}%; transform: translate(${xShift}, ${yShift});`;
    },

    metricPointLabel(chart, point) {
      return `#${point.height}: ${this.metricValueLabel(chart, point.value)}`;
    },

    metricAxisValueLabel(chart, value) {
      const number = Number(value);
      if (!Number.isFinite(number)) return "-";
      if (chart?.valueKind === "seconds") return `${this.compactNumber(number)}s`;
      return this.compactNumber(number);
    },

    metricValueLabel(chart, value) {
      const number = Number(value);
      if (!Number.isFinite(number)) return "-";
      if (chart?.valueKind === "iuna") return `IUNA ${this.compactNumber(number)}`;
      if (chart?.valueKind === "seconds") return `${this.compactNumber(number)} s`;
      return `${this.compactNumber(number)}${chart?.unit ? ` ${chart.unit}` : ""}`;
    },

    compactNumber(value) {
      const number = Number(value);
      if (!Number.isFinite(number)) return "-";
      if (Math.abs(number) >= 1000) {
        return new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(number);
      }
      if (Number.isInteger(number)) return String(number);
      return number.toFixed(6).replace(/0+$/, "").replace(/\.$/, "");
    },

    amountLabel(value) {
      const microiuna = Math.max(0, Math.trunc(Number(value) || 0));
      const whole = Math.floor(microiuna / 1000000);
      const fractional = String(microiuna % 1000000).padStart(6, "0").replace(/0+$/, "");
      return fractional ? `${whole}.${fractional}` : `${whole}`;
    },

    metricAmountLabel(value) {
      return value === null || value === undefined ? "-" : `IUNA ${this.amountLabel(value)}`;
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
        this.selectedTransferUtxoAmounts = {};
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

    showSettingsFeedback(message, kind) {
      this.settingsFeedback = { message, kind };
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

    spendableWalletUtxos() {
      return this.walletUtxos.filter((utxo) => utxo.spendable !== false);
    },

    rememberUtxoAmounts(utxos) {
      for (const utxo of utxos || []) {
        this.selectedTransferUtxoAmounts[this.utxoOutpoint(utxo)] = Number(utxo.amount || 0);
      }
    },

    pruneSelectedTransferUtxos() {
      const visible = new Map(this.walletUtxos.map((utxo) => [this.utxoOutpoint(utxo), utxo]));
      this.selectedTransferUtxos = this.selectedTransferUtxos.filter((outpoint) => {
        const utxo = visible.get(outpoint);
        return !utxo || utxo.spendable !== false;
      });
    },

    async selectAllTransferUtxos() {
      try {
        const utxos = await this.fetchJson("/api/wallet/utxos/selectable");
        this.rememberUtxoAmounts(utxos);
        this.selectedTransferUtxos = utxos.map((utxo) => this.utxoOutpoint(utxo));
        this.scheduleFeeEstimates();
        if (this.selectedTransferUtxos.length === 0) {
          this.showFlash("No spendable UTXOs", "error");
        }
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    clearTransferUtxos() {
      this.selectedTransferUtxos = [];
      this.selectedTransferUtxoAmounts = {};
      this.scheduleFeeEstimates();
    },

    selectedTransferUtxoTotal() {
      return this.selectedTransferUtxos.reduce((sum, outpoint) => {
        return sum + Number(this.selectedTransferUtxoAmounts[outpoint] || 0);
      }, 0);
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
      const owner = block.miner === this.status.wallet_address ? `${finalizer} (me)` : finalizer;
      return block.finalizer_mode === "recovery" ? `${owner} · Recovery` : owner;
    },

    burnLeaderRanks(block) {
      if (Array.isArray(block?.burn_leader_ranks)) return block.burn_leader_ranks;
      return Array.isArray(block?.burnLeaderRanks) ? block.burnLeaderRanks : [];
    },

    burnLeaderRanksTitle(block) {
      if (!block) return "Burn Leader Ranks";
      return `Block ${block.height} Burn Leader Ranks`;
    },

    burnLeaderRankLabel(rank) {
      const value = Number(rank?.rank ?? 0);
      return `#${value + 1}`;
    },

    burnLeaderEligibilityLabel(rank) {
      const from = rank?.eligible_from_height ?? rank?.eligibleFromHeight ?? "-";
      const until = rank?.eligible_until_height ?? rank?.eligibleUntilHeight ?? "-";
      return `${from}-${until}`;
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

    networkHealthClass() {
      if (this.networkHealth.ok) return "healthy";
      if (this.networkHealth.state === "syncing") return "syncing";
      if (this.networkHealth.state === "isolated") return "isolated";
      if (this.networkHealth.state === "stale") return "stale";
      if (this.networkHealth.state === "banned") return "banned";
      return "error";
    },

    networkLagLabel() {
      const lag = this.networkHealth.lag_blocks;
      if (typeof lag !== "number") return "-";
      if (lag === 0) return "even";
      return `${lag} behind`;
    },

    basicNetworkStatusLabel() {
      const state = this.networkHealth.state;
      if (!state) return "Network starting";
      if (state === "healthy" || state === "ahead of peers") return "Connected";
      if (state === "syncing" || state === "mempool syncing") return "Syncing";
      if (state === "isolated") return "Offline";
      return state.charAt(0).toUpperCase() + state.slice(1);
    },

    basicNetworkNeedsAttention() {
      if (!this.networkHealth.state) return false;
      return !this.networkHealth.ok && this.networkHealth.state !== "syncing";
    },

    networkTimeOffsetLabel() {
      return this.clockOffsetLabel(this.networkHealth.network_time_offset_ms, true);
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

    stalePeer(peer) {
      const lastSuccess = peer.last_success_ms;
      if (typeof lastSuccess !== "number") return false;
      return Date.now() - lastSuccess > 20 * 60 * 1000;
    },

    bannedPeer(peer) {
      const bannedUntil = peer.banned_until_ms;
      return typeof bannedUntil === "number" && bannedUntil > Date.now();
    },

    peerStatus(peer) {
      if (this.bannedPeer(peer)) return "banned";
      if (peer.last_error) return "error";
      if (this.stalePeer(peer)) return "stale";
      if (typeof peer.last_known_height === "number") return "synced";
      if ((peer.messages_sent ?? 0) > 0 || (peer.messages_received ?? 0) > 0) return "active";
      return "pending";
    },

    peerStatusLabel(peer) {
      return {
        error: "Error",
        banned: "Banned",
        stale: "Stale",
        synced: "Synced",
        active: "Active",
        pending: "Pending",
      }[this.peerStatus(peer)];
    },

    relativeTimeLabel(timestampMs) {
      if (typeof timestampMs !== "number") return "-";
      const ageSeconds = Math.max(0, Math.round((Date.now() - timestampMs) / 1000));
      if (ageSeconds < 5) return "now";
      if (ageSeconds < 60) return `${ageSeconds}s ago`;
      const ageMinutes = Math.round(ageSeconds / 60);
      if (ageMinutes < 60) return `${ageMinutes}m ago`;
      const ageHours = Math.round(ageMinutes / 60);
      if (ageHours < 48) return `${ageHours}h ago`;
      return `${Math.round(ageHours / 24)}d ago`;
    },

    peerLastContactLabel(peer) {
      return this.relativeTimeLabel(peer.last_contact_ms);
    },

    peerClockLabel(peer) {
      const label = this.clockOffsetLabel(peer.last_clock_offset_ms, false);
      if (label === "-") return "-";
      return peer.last_clock_offset_accepted === false ? `${label} ignored` : label;
    },

    clockOffsetLabel(offsetMs, zeroAsSynced) {
      if (typeof offsetMs !== "number") return "-";
      const sign = offsetMs > 0 ? "+" : offsetMs < 0 ? "-" : "";
      const absoluteSeconds = Math.round(Math.abs(offsetMs) / 1000);
      if (absoluteSeconds === 0) return zeroAsSynced ? "even" : "0s";
      if (absoluteSeconds < 60) return `${sign}${absoluteSeconds}s`;
      const minutes = Math.round(absoluteSeconds / 60);
      if (minutes < 60) return `${sign}${minutes}m`;
      return `${sign}${Math.round(minutes / 60)}h`;
    },

    peerBanLabel(peer) {
      if (!this.bannedPeer(peer)) return "-";
      const remainingSeconds = Math.max(0, Math.round((peer.banned_until_ms - Date.now()) / 1000));
      if (remainingSeconds < 60) return `${remainingSeconds}s`;
      const remainingMinutes = Math.round(remainingSeconds / 60);
      if (remainingMinutes < 60) return `${remainingMinutes}m`;
      return `${Math.round(remainingMinutes / 60)}h`;
    },

    normalizeVersion(version) {
      return String(version || "").trim().replace(/^v/i, "");
    },

    versionParts(version) {
      const [core] = this.normalizeVersion(version).split("-");
      return core.split(".").map((part) => Number.parseInt(part, 10) || 0);
    },

    compareVersions(left, right) {
      const leftParts = this.versionParts(left);
      const rightParts = this.versionParts(right);
      const length = Math.max(leftParts.length, rightParts.length, 3);
      for (let index = 0; index < length; index += 1) {
        const diff = (leftParts[index] || 0) - (rightParts[index] || 0);
        if (diff !== 0) return diff;
      }
      return 0;
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
