const IUNA_DOWNLOADS_URL = "https://getiuna.org/downloads/";
const IUNA_RELEASE_METADATA_URL = "https://getiuna.org/downloads/latest.json";
const IUNA_RELEASE_CHECK_INTERVAL_MS = 30 * 60 * 1000;
// Matches the node's default burn amount, in micro-IUNA.
const IUNA_LOW_BURN_AMOUNT = 100;

window.iunaApp = function iunaApp() {
  return {
    tab: "dashboard",
    commandKeyHeld: false,
    status: {},
    blocks: [],
    selectedBlock: null,
    selectedByteBlock: null,
    selectedBurnBundleBlock: null,
    selectedTransaction: null,
    selectedBurnLeaderBlock: null,
    burnLeaderRanksError: null,
    burnLeaderRanksRefreshPromise: null,
    burnLeaderRanksRefreshTimer: null,
    loadingInitialBlocks: false,
    loadingOlder: false,
    hasMoreBlocks: true,
    walletTxs: [],
    walletUtxos: [],
    walletAction: "send",
    mempool: [],
    peers: [],
    selectedPeerAddress: null,
    p2pMetrics: {},
    blockchainMetrics: { enabled: false, latest: null, charts: [] },
    loadingMetrics: false,
    metricsRequestSeq: 0,
    metricHover: null,
    leaderboardTab: "balances",
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
    networkHealthLoaded: false,
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
    desktopUpdateModalOpen: false,
    desktopUpdateBusy: false,
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
    vdfMemoryMib: 256,
    addressBook: {},
    addressBookVersion: 0,
    addressBookModalOpen: false,
    addressBookPickerOpen: false,
    addressBookStandalone: false,
    addressBookOverview: false,
    addressBookEditingAddress: null,
    addressBookDraftAddress: "",
    addressBookDraftName: "",
    p2pAcceptInbound: false,
    p2pBindPort: 9444,
    p2pBindPortDirty: false,
    p2pAnnounceAddr: "",
    p2pAnnounceDirty: false,
    stratumEnabled: false,
    stratumBindPort: 3333,
    stratumBindPortDirty: false,
    walletEndpointEnabled: false,
    walletEndpointBindPort: 18662,
    walletEndpointBindPortDirty: false,
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
    migrationBusy: false,
    burnAmount: 100,
    burnAmountDraft: "0.0001",
    burnFee: 1,
    burnFeeDraft: "0.000001",
    miningEnabled: false,
    powMiningEnabled: false,
    powMiningWorkers: 1,
    maxPowMiningWorkers: 32,
    recoveryVdfTopRankPercent: 50,
    burnAmountDirty: false,
    miningEvents: [],
    miningEventLimit: 1000,
    miningEventState: {},
    miningEventCounter: 0,
    transferTo: "",
    transferAmount: null,
    transferFee: "0.000001",
    sendPreparing: false,
    sendConfirmModalOpen: false,
    sendConfirmBusy: false,
    pendingTransfer: null,
    feeEstimates: { transfer: null, burn: null, mine: null },
    feeEstimateTimer: null,
    showSendAdvanced: false,
    optimizeOpen: false,
    optimizeBusy: false,
    optimizeRunning: false,
    optimizeFee: "0.000001",
    optimizeMergeRoots: false,
    optimizePlan: null,
    optimizeMessage: "",
    optimizeError: "",
    optimizeDismissed: false,
    quantumMigrationFee: "0.000001",
    quantumMigrationBusy: false,
    quantumMigrationSubmitting: false,
    quantumMigrationPreview: null,
    quantumMigrationError: "",
    selectedTransferUtxos: [],
    selectedTransferUtxoAmounts: {},
    lastSelectedTransferUtxo: null,
    walletTxFilters: { transfer: true, mine: true, burn: true, reward: true },
    setupPeerAddress: "iuna.jhx.app:9444",
    peerAddress: "",
    flash: null,
    flashTimer: null,
    chainResetModalOpen: false,
    chainResetConfirm: "",
    chainResetBusy: false,
    showWalletUtxos: false,
    showWalletAddresses: false,
    showPowDifficultyInfo: false,
    lastUpdated: null,
    pollHandle: null,
    releaseCheckTimer: null,
    refreshPromise: null,
    shellRefreshPromise: null,
    networkHealthPromise: null,
    requestTimeoutMs: 12000,
    hashListenerInstalled: false,
    newBlockHashes: new Set(),
    newBlockTimer: null,
    lastBlockMempoolHeight: null,
    mempoolFirstSeenHeights: {},
    mempoolFirstSeenAt: {},
    mempoolSeenInitialized: false,
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
          this.setTab(this.tabFromHash());
        });
        this.hashListenerInstalled = true;
      }
      await this.refresh();
      this.checkLatestRelease();
      this.scheduleReleaseCheck();
      this.schedulePoll();
    },

    canUseProtectedApi() {
      return this.authLoaded && this.auth.configured === true && this.auth.authenticated === true;
    },

    stopPolling() {
      this.stopReleaseCheck();
      if (!this.pollHandle) return;
      clearTimeout(this.pollHandle);
      this.pollHandle = null;
    },

    stopReleaseCheck() {
      if (!this.releaseCheckTimer) return;
      clearTimeout(this.releaseCheckTimer);
      this.releaseCheckTimer = null;
    },

    scheduleReleaseCheck() {
      if (this.releaseCheckTimer || !this.canUseProtectedApi()) return;
      this.releaseCheckTimer = setTimeout(async () => {
        this.releaseCheckTimer = null;
        if (!this.canUseProtectedApi()) return;
        await this.checkLatestRelease();
        this.scheduleReleaseCheck();
      }, IUNA_RELEASE_CHECK_INTERVAL_MS);
    },

    schedulePoll() {
      if (this.pollHandle || !this.canUseProtectedApi()) return;
      const delay = this.syncingNode() ? 1000 : 5000;
      this.pollHandle = setTimeout(async () => {
        this.pollHandle = null;
        await this.refresh({ silent: true });
        this.schedulePoll();
      }, delay);
    },

    tabFromHash() {
      const hash = window.location.hash.replace(/^#\/?/, "");
      return this.allowedTabs().includes(hash) ? hash : "dashboard";
    },

    setTab(tab) {
      if (!this.allowedTabs().includes(tab)) return;
      const alreadyActive = this.tab === tab;
      this.tab = tab;
      if (window.location.hash !== `#${tab}`) {
        window.location.hash = tab;
      }
      if (alreadyActive) return;
      this.refresh({ silent: true });
    },

    allowedTabs() {
      const tabs = this.advancedMode()
        ? ["dashboard", "wallet", "mining", "p2p", "chain", "settings"]
        : ["dashboard", "wallet", "p2p", "chain", "settings"];
      if (this.developmentMode()) {
        tabs.splice(tabs.indexOf("chain") + 1, 0, "metrics", "leaderboards");
      }
      return tabs;
    },

    navigationShortcutNumber(tab) {
      const index = this.allowedTabs().indexOf(tab);
      return index >= 0 ? String(index + 1) : "";
    },

    navigationShortcutsAvailable() {
      return (
        this.canUseProtectedApi() &&
        this.config.setup_complete === true &&
        !this.showingNetworkMigration() &&
        !this.syncingNode()
      );
    },

    handleNavigationKeydown(event) {
      if (event.key === "Meta") {
        this.commandKeyHeld = this.navigationShortcutsAvailable();
        return;
      }
      if (
        !event.metaKey ||
        event.altKey ||
        event.ctrlKey ||
        event.shiftKey ||
        !this.navigationShortcutsAvailable()
      ) return;
      this.commandKeyHeld = true;
      const keyNumber = /^[1-9]$/.test(event.key || "")
        ? Number(event.key)
        : /^Digit[1-9]$/.test(event.code || "")
          ? Number(event.code.slice(-1))
          : 0;
      const tab = this.allowedTabs()[keyNumber - 1];
      if (!tab) return;
      event.preventDefault();
      this.closeModals();
      this.setTab(tab);
    },

    handleNavigationKeyup(event) {
      if (event.key === "Meta" || !event.metaKey) {
        this.commandKeyHeld = false;
      }
    },

    releaseCommandKey() {
      this.commandKeyHeld = false;
    },

    developmentMode() {
      return this.keepTrackOfMetrics === true || this.config.keep_track_of_metrics === true;
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
        this.setTab("dashboard");
      }
    },

    toggleUiMode() {
      this.setUiMode(this.advancedMode() ? "basic" : "advanced");
    },

    pageTitle() {
      return {
        dashboard: "Dashboard",
        wallet: "Wallet",
        mining: "Mining",
        p2p: "P2P",
        chain: "Chain",
        metrics: "Metrics",
        leaderboards: "Leaderboards",
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
      const current = this.normalizeVersion(this.status.app_version);
      const latest = this.normalizeVersion(this.latestRelease?.tag);
      if (!current || !latest) return false;
      if (current === latest) return false;
      return this.compareVersions(latest, current) > 0;
    },

    versionPanelTitle() {
      if (this.desktopUpdateBusy) return "Installing update";
      if (this.updateAvailable()) return `Update available: ${this.latestReleaseLabel()}`;
      if (this.releaseCheckState === "failed") return this.releaseCheckError || "Could not check latest release";
      if (this.releaseCheckState === "checking") return "Checking latest release";
      return "iuna is up to date";
    },

    async openLatestRelease() {
      const url = this.latestRelease?.url || IUNA_DOWNLOADS_URL;
      const invoke = window.__TAURI__?.core?.invoke;
      if (
        this.updateAvailable()
        && this.latestRelease?.desktopReady === true
        && typeof invoke === "function"
      ) {
        this.desktopUpdateModalOpen = true;
        return;
      }
      try {
        const tauriOpen = window.__TAURI__?.shell?.open;
        if (typeof tauriOpen === "function") {
          await tauriOpen(url);
          return;
        }
      } catch {}
      window.open(url, "_blank", "noopener,noreferrer");
    },

    closeDesktopUpdateModal() {
      if (this.desktopUpdateBusy) return;
      this.desktopUpdateModalOpen = false;
    },

    async installDesktopUpdate() {
      const invoke = window.__TAURI__?.core?.invoke;
      if (typeof invoke !== "function") return;
      this.desktopUpdateBusy = true;
      try {
        await invoke("install_desktop_update");
      } catch (error) {
        this.desktopUpdateBusy = false;
        this.desktopUpdateModalOpen = false;
        this.showFlash(error?.message || String(error) || "Desktop update failed", "error");
      }
    },

    showingSetup() {
      return this.authLoaded && !this.showingAuth() && !this.showingNetworkMigration() && !this.config.setup_complete;
    },

    showingNetworkMigration() {
      return this.authLoaded && !this.showingAuth() && this.status.network_migration?.required === true;
    },

    migrationNetworkLabel() {
      return this.status.network_migration?.to_network || "the new Iuna network";
    },

    async finishNetworkMigration(setUpDifferentWallet) {
      this.migrationBusy = true;
      try {
        await this.submitForm("/api/settings/chain-reset", { confirm: "RESET" });
        if (setUpDifferentWallet) {
          const response = await this.fetchWithTimeout("/api/config", {
            method: "POST",
            headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
            body: new URLSearchParams({ setup_complete: "false", peer: "" }),
          });
          const payload = await response.json();
          if (!response.ok || !payload.ok) throw new Error(payload.error || "Could not open wallet setup");
          this.config.setup_complete = false;
        }
        await this.refresh({ force: true });
        this.showFlash(setUpDifferentWallet ? "Local chain reset. Choose or import a wallet." : "Local chain reset. Sync requested from peers.", "success");
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        this.migrationBusy = false;
      }
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
      const response = await this.fetchWithTimeout(path, {
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
        this.stopPolling();
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
        const response = await this.fetchWithTimeout("/api/auth/change-password", {
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
      this.syncConfigState({ addressBookVersion: this.addressBookVersion });
    },

    syncConfigState(options = {}) {
      this.keepTrackOfMetrics = this.config.keep_track_of_metrics === true;
      this.recoveryVdfTopRankPercent = Number(
        this.config.recovery_vdf_top_rank_percent ??
          this.config.recoveryVdfTopRankPercent ??
          this.recoveryVdfTopRankPercent
      );
      this.vdfMemoryMib = Number(this.config.vdf_memory_mib || 256);
      this.p2pAcceptInbound = this.config.p2p_accept_inbound === true;
      if (!this.p2pBindPortDirty) {
        this.p2pBindPort = Number(this.config.p2p_bind_port || 9444);
      }
      this.stratumEnabled = this.config.stratum_enabled === true;
      if (!this.stratumBindPortDirty) {
        this.stratumBindPort = Number(this.config.stratum_bind_port || 3333);
      }
      this.walletEndpointEnabled = this.config.wallet_endpoint_enabled === true;
      if (!this.walletEndpointBindPortDirty) {
        this.walletEndpointBindPort = Number(this.config.wallet_endpoint_bind_port || 18662);
      }
      if (
        options.addressBookVersion === undefined ||
        options.addressBookVersion >= this.addressBookVersion
      ) {
        this.addressBook = this.config.address_book || this.config.addressBook || {};
      }
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
      return this.setupWallet.address || this.status.wallet_receive_address || "-";
    },

    receiveAddress() {
      return this.status.wallet_receive_address || this.setupWallet.address || "-";
    },

    fundedWalletAddresses() {
      return Array.isArray(this.status.funded_wallet_addresses)
        ? this.status.funded_wallet_addresses
        : [];
    },

    walletAddressSummary() {
      const count = this.fundedWalletAddresses().length;
      return `Funds are held across ${count} wallet address${count === 1 ? "" : "es"}`;
    },

    walletAddressHasPendingSpend(entry) {
      return Number(entry?.spendable_utxos || 0) < Number(entry?.utxos || 0);
    },

    walletAddressState(entry) {
      if (this.walletAddressHasPendingSpend(entry)) return "Pending spend";
      if (entry?.address === this.receiveAddress()) return "Current";
      if (entry?.legacy === true) return "Legacy funds";
      return "Funded";
    },

    walletAddressUtxoLabel(entry) {
      const total = Number(entry?.utxos || 0);
      const available = Number(entry?.spendable_utxos || 0);
      const pending = Math.max(0, total - available);
      const parts = [`${total} UTXO${total === 1 ? "" : "s"}`];
      if (pending > 0) parts.push(`${available} available`, `${pending} pending`);
      return parts.join(" · ");
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
        await this.refresh({ force: true });
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
        await this.refresh({ force: true });
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
      const response = await this.fetchWithTimeout(path, {
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
        const response = await this.fetchWithTimeout("/api/config", {
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
        await this.refresh({ force: true });
        this.setupFeedback = null;
        this.generatedSeedPhrase = "";
        this.importSeedPhrase = "";
        this.setupPeerAddress = "";
        this.verifyChallenges = [];
        this.verifyAnswers = {};
        this.showFlash("Setup complete", "success");
        this.setTab("dashboard");
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
        await this.submitForm("/api/settings/p2p-inbound", {
          enabled: acceptInbound,
          bind_port: this.p2pBindPortValue(),
        });
        this.p2pAcceptInbound = acceptInbound;
      }
      this.setUiMode(mode === "wallet" ? "basic" : "advanced");
    },

    async refresh(options = {}) {
      if (this.refreshPromise) {
        if (options.force === true) {
          try {
            await this.refreshPromise;
          } catch {
            // The forced refresh below should report the current state.
          }
        } else {
          return this.refreshPromise;
        }
      }
      this.refreshPromise = this.refreshNow(options).finally(() => {
        this.refreshPromise = null;
      });
      return this.refreshPromise;
    },

    async refreshNow(options = {}) {
      if (!this.canUseProtectedApi()) return;
      const addressBookVersion = this.addressBookVersion;
      const tab = this.tab;
      const shouldLoadBlocks = tab === "chain" || tab === "mining";
      const shouldLoadP2pMetrics = tab === "p2p" && this.developmentMode();
      const shouldLoadMetrics = tab === "metrics" || tab === "leaderboards";
      if (shouldLoadMetrics) {
        await this.refreshMetrics(options);
        this.refreshShellState({ addressBookVersion, silent: true });
        return;
      }
      if (shouldLoadBlocks && this.blocks.length === 0) this.loadingInitialBlocks = true;
      const pagedDatasets = [];
      if (tab === "wallet") pagedDatasets.push("walletTx", "walletUtxo");
      if (tab === "chain") pagedDatasets.push("mempool");
      if (tab === "p2p") pagedDatasets.push("peer");
      try {
        const [config, status, networkHealth, blocks, p2pMetrics, blockchainMetrics] = await Promise.all([
          this.fetchJson("/api/config"),
          this.fetchJson("/api/status"),
          this.refreshNetworkHealth({ silent: options.silent === true }),
          shouldLoadBlocks
            ? this.fetchJson("/api/blocks?limit=30").catch((error) => {
                if (error.uiDataLoading) return null;
                throw error;
              })
            : Promise.resolve(null),
          shouldLoadP2pMetrics ? this.fetchJson("/api/p2p/metrics") : Promise.resolve(this.p2pMetrics),
          Promise.resolve(this.blockchainMetrics),
        ]);
        const previousChainHeight = this.status.chain?.height;
        this.status = status;
        if (networkHealth) {
          this.networkHealth = networkHealth;
          this.networkHealthLoaded = true;
        }
        this.config = config;
        this.syncConfigState({ addressBookVersion });
        if (!this.allowedTabs().includes(this.tab)) {
          this.setTab("dashboard");
        }
        if (!this.config.setup_complete || status.network_migration?.required === true) {
          await this.refreshWalletSetup();
        }
        this.syncMempoolBlockMarker(previousChainHeight, status.chain?.height);
        if (blocks) this.mergeFreshBlocks(blocks, { animateHead: true });
        this.pruneSelectedTransferUtxos();
        this.p2pMetrics = p2pMetrics;
        this.blockchainMetrics = blockchainMetrics;
        this.burnAmount = status.mining?.burn_per_block ?? this.burnAmount;
        this.burnFee = status.mining?.automatic_burn_fee ?? this.burnFee;
        this.miningEnabled = status.mining?.automatic ?? this.miningEnabled;
        this.powMiningEnabled = status.mining?.pow_mining_enabled ?? this.powMiningEnabled;
        this.powMiningWorkers = status.mining?.pow_mining_workers ?? this.powMiningWorkers;
        this.maxPowMiningWorkers =
          status.mining?.max_pow_mining_workers ?? this.maxPowMiningWorkers;
        if (!this.burnAmountDirty) {
          this.burnAmountDraft = this.amountLabel(this.burnAmount);
          this.burnFeeDraft = this.amountLabel(this.burnFee);
        }
        this.lastUpdated = new Date();
        this.syncMiningEvents({ status, blocks });
        this.scheduleFeeEstimates();
        await Promise.all(
          pagedDatasets.map((kind) =>
            this.refreshPagedDataset(kind, { silent: options.silent === true })
          )
        );
      } catch (error) {
        if (String(error.message || "").includes("401")) {
          this.stopPolling();
          await this.refreshAuth();
          return;
        }
        if (!error.uiDataLoading) this.showFlash(error.message, "error");
      } finally {
        if (shouldLoadBlocks) this.loadingInitialBlocks = false;
      }
    },

    async refreshShellState(options = {}) {
      if (!this.canUseProtectedApi()) return;
      if (this.shellRefreshPromise) return this.shellRefreshPromise;
      const addressBookVersion = options.addressBookVersion ?? this.addressBookVersion;
      this.shellRefreshPromise = Promise.all([
        this.fetchJson("/api/config"),
        this.fetchJson("/api/status"),
        this.refreshNetworkHealth({ silent: true }),
      ])
        .then(async ([config, status, networkHealth]) => {
          const previousChainHeight = this.status.chain?.height;
          this.status = status;
          if (networkHealth) {
            this.networkHealth = networkHealth;
            this.networkHealthLoaded = true;
          }
          this.config = config;
          this.syncConfigState({ addressBookVersion });
          if (!this.allowedTabs().includes(this.tab)) {
            this.setTab("dashboard");
          }
          if (!this.config.setup_complete) {
            await this.refreshWalletSetup();
          }
          this.syncMempoolBlockMarker(previousChainHeight, status.chain?.height);
          this.burnAmount = status.mining?.burn_per_block ?? this.burnAmount;
          this.burnFee = status.mining?.automatic_burn_fee ?? this.burnFee;
          this.miningEnabled = status.mining?.automatic ?? this.miningEnabled;
          this.powMiningEnabled = status.mining?.pow_mining_enabled ?? this.powMiningEnabled;
          this.powMiningWorkers = status.mining?.pow_mining_workers ?? this.powMiningWorkers;
          this.maxPowMiningWorkers =
            status.mining?.max_pow_mining_workers ?? this.maxPowMiningWorkers;
          if (!this.burnAmountDirty) {
            this.burnAmountDraft = this.amountLabel(this.burnAmount);
            this.burnFeeDraft = this.amountLabel(this.burnFee);
          }
          this.lastUpdated = new Date();
          this.syncMiningEvents({ status, blocks: null });
          this.scheduleFeeEstimates();
        })
        .catch((error) => {
          if (options.silent !== true) this.showFlash(error.message, "error");
        })
        .finally(() => {
          this.shellRefreshPromise = null;
        });
      return this.shellRefreshPromise;
    },

    async refreshNetworkHealth(options = {}) {
      if (!this.canUseProtectedApi()) return null;
      if (this.networkHealthPromise) return this.networkHealthPromise;
      this.networkHealthPromise = this.fetchJson("/api/network/health")
        .then((networkHealth) => networkHealth)
        .catch((error) => {
          if (options.silent !== true) this.showFlash(error.message, "error");
          return null;
        })
        .finally(() => {
          this.networkHealthPromise = null;
        });
      return this.networkHealthPromise;
    },

    async fetchJson(path) {
      const response = await this.fetchWithTimeout(path, {
        headers: { Accept: "application/json" },
        cache: "no-store",
      });
      if (response.status === 503) {
        const error = new Error("Chain data is still loading");
        error.uiDataLoading = true;
        throw error;
      }
      if (!response.ok) {
        throw new Error(`${path} returned ${response.status}`);
      }
      return response.json();
    },

    uiDataLoading() {
      return this.status.ui_data_ready === false;
    },

    async fetchWithTimeout(path, options = {}) {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), this.requestTimeoutMs);
      try {
        return await fetch(path, { ...options, signal: controller.signal });
      } catch (error) {
        if (error?.name === "AbortError") {
          throw new Error(`${path} timed out`);
        }
        throw error;
      } finally {
        clearTimeout(timeout);
      }
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
      this.resetPageState(kind);
      await this.refreshPagedDataset(kind);
    },

    resetPageState(kind) {
      const config = this.datasetConfig(kind);
      if (!config) return;
      Object.assign(this[config.page], {
        offset: 0,
        total: 0,
        hasMore: true,
        loading: false,
        backgroundLoading: false,
      });
    },

    async refreshPagedDataset(kind, options = {}) {
      if (!this.canUseProtectedApi()) return;
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
      if (!this.canUseProtectedApi()) return;
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
        if (kind === "mempool") {
          this.trackMempoolFirstSeenHeights({ append: options.replace !== true });
          this.sortMempoolNewestFirst();
        }
        page.offset = normalized.nextOffset ?? this[config.items].length;
        page.total = normalized.total;
        page.hasMore = normalized.hasMore;
        if (kind === "walletUtxo") {
          this.rememberUtxoAmounts(this.walletUtxos);
          this.pruneSelectedTransferUtxos();
        }
      } catch (error) {
        if (!error.uiDataLoading) this.showFlash(error.message, "error");
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

    syncMempoolBlockMarker(previousHeight, currentHeight) {
      const normalizedCurrent = Number(currentHeight);
      if (!Number.isFinite(normalizedCurrent)) return;
      const normalizedPrevious = Number(previousHeight);
      if (this.lastBlockMempoolHeight === null) {
        this.lastBlockMempoolHeight = normalizedCurrent;
        return;
      }
      if (!Number.isFinite(normalizedPrevious) || normalizedCurrent > normalizedPrevious) {
        this.lastBlockMempoolHeight = normalizedCurrent;
      }
    },

    trackMempoolFirstSeenHeights(options = {}) {
      const height = Number(this.status.chain?.height);
      if (!Number.isFinite(height)) return;
      const active = new Set();
      const firstBatch = !this.mempoolSeenInitialized;
      const seenHeight = firstBatch ? height - 1 : height;
      const knownSeenTimes = Object.values(this.mempoolFirstSeenAt)
        .map((value) => Number(value))
        .filter((value) => Number.isFinite(value));
      const oldestSeenAt = knownSeenTimes.length ? Math.min(...knownSeenTimes) : Date.now();
      const baseSeenAt = options.append && this.mempoolSeenInitialized
        ? oldestSeenAt - 1
        : Date.now();
      let newIndex = 0;
      for (const tx of this.mempool) {
        const key = this.mempoolKey(tx);
        if (!key) continue;
        active.add(key);
        if (this.mempoolFirstSeenHeights[key] === undefined) {
          this.mempoolFirstSeenHeights[key] = seenHeight;
          this.mempoolFirstSeenAt[key] = baseSeenAt - newIndex;
          newIndex += 1;
        }
      }
      this.mempoolSeenInitialized = true;
      for (const key of Object.keys(this.mempoolFirstSeenHeights)) {
        if (!active.has(key)) {
          delete this.mempoolFirstSeenHeights[key];
          delete this.mempoolFirstSeenAt[key];
        }
      }
    },

    mempoolKey(tx) {
      return tx?.signature || "";
    },

    mempoolItemClass(tx) {
      const key = this.mempoolKey(tx);
      const firstSeenHeight = Number(this.mempoolFirstSeenHeights[key]);
      const markerHeight = Number(this.status.chain?.height ?? this.lastBlockMempoolHeight);
      const classes = [];
      if (key && Number.isFinite(firstSeenHeight) && Number.isFinite(markerHeight)) {
        classes.push(firstSeenHeight >= markerHeight ? "new-since-block" : "before-last-block");
      }
      return classes.join(" ");
    },

    mempoolSeenTimeLabel(tx) {
      const seenAt = Number(this.mempoolFirstSeenAt[this.mempoolKey(tx)]);
      if (!Number.isFinite(seenAt)) return "";
      return `Seen ${new Date(seenAt).toLocaleTimeString()}`;
    },

    sortMempoolNewestFirst() {
      this.mempool = [...this.mempool].sort((left, right) => {
        const leftSeenAt = Number(this.mempoolFirstSeenAt[this.mempoolKey(left)]);
        const rightSeenAt = Number(this.mempoolFirstSeenAt[this.mempoolKey(right)]);
        if (Number.isFinite(leftSeenAt) && Number.isFinite(rightSeenAt) && leftSeenAt !== rightSeenAt) {
          return rightSeenAt - leftSeenAt;
        }
        const leftSeen = Number(this.mempoolFirstSeenHeights[this.mempoolKey(left)]);
        const rightSeen = Number(this.mempoolFirstSeenHeights[this.mempoolKey(right)]);
        if (Number.isFinite(leftSeen) && Number.isFinite(rightSeen) && leftSeen !== rightSeen) {
          return rightSeen - leftSeen;
        }
        return this.mempoolKey(right).localeCompare(this.mempoolKey(left));
      });
    },

    observePageSentinel(kind, element) {
      if (!element || element.__iunaPageObserver) return;
      const observer = new IntersectionObserver((entries) => {
        if (this.canUseProtectedApi() && entries.some((entry) => entry.isIntersecting)) {
          this.loadNextPage(kind);
        }
      }, { root: null, rootMargin: "180px 0px" });
      observer.observe(element);
      element.__iunaPageObserver = observer;
    },

    observeBlockSentinel(element) {
      if (!element || element.__iunaBlockObserver) return;
      const observer = new IntersectionObserver((entries) => {
        if (this.canUseProtectedApi() && entries.some((entry) => entry.isIntersecting)) {
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
        reward: String(this.walletTxFilters.reward),
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
        const response = await fetch(IUNA_RELEASE_METADATA_URL, {
          cache: "no-store",
          headers: { Accept: "application/json" },
        });
        if (!response.ok) {
          throw new Error(`Release check failed (${response.status})`);
        }
        const release = await response.json();
        const version = this.normalizeVersion(release.tag || release.version);
        if (!version) {
          throw new Error("Release metadata is missing a version");
        }
        let desktopVersion = null;
        const invoke = window.__TAURI__?.core?.invoke;
        if (typeof invoke === "function") {
          try {
            desktopVersion = this.normalizeVersion(await invoke("check_desktop_update"));
          } catch {}
        }
        this.latestRelease = {
          tag: `v${version}`,
          url: release.url || IUNA_DOWNLOADS_URL,
          desktopReady: desktopVersion === version,
        };
        this.releaseCheckState = "done";
      } catch (error) {
        this.releaseCheckError = error.message || "Release check failed";
        this.releaseCheckState = "failed";
      }
    },

    mergeFreshBlocks(freshBlocks, options = {}) {
      const hadBlocks = this.blocks.length > 0;
      const previousHeights = new Set(this.blocks.map((block) => block.height));
      const previousHead = this.blocks[0]?.height;
      const previousHeadHash = this.blocks[0]?.hash;
      const wasFollowingHead =
        !this.selectedBlock || (previousHeadHash && this.selectedBlock.hash === previousHeadHash);
      const rail = this.$refs.blockRail;
      const previousScrollWidth = hadBlocks ? rail?.scrollWidth ?? 0 : 0;
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
      if (this.selectedBurnLeaderBlock) {
        this.selectedBurnLeaderBlock = known.get(this.selectedBurnLeaderBlock.hash) || null;
        if (!this.burnLeaderRanksPending(this.selectedBurnLeaderBlock)) {
          this.burnLeaderRanksError = null;
          this.stopBurnLeaderRanksRefresh();
        } else if (!this.burnLeaderRanksError) {
          this.scheduleBurnLeaderRanksRefresh(this.syncingNode() ? 1000 : 5000);
        }
      }
      this.hasMoreBlocks =
        this.blocks.some((block) => block.height > 0) &&
        !this.blocks.some((block) => block.height === 0);

      const newHeadBlocks = options.animateHead
        && hadBlocks
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
      } else if (!hadBlocks) {
        this.$nextTick(() => this.resetBlockRailPosition());
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

    resetBlockRailPosition() {
      const rail = this.$refs.blockRail;
      if (!rail) return;
      rail.scrollLeft = 0;
    },

    selectBlock(block) {
      this.selectedBlock = block;
    },

    openBurnLeaderRanksModal(block) {
      this.stopBurnLeaderRanksRefresh();
      this.burnLeaderRanksError = null;
      this.selectedBurnLeaderBlock = block;
      this.scheduleBurnLeaderRanksRefresh(0);
    },

    closeBurnLeaderRanksModal() {
      this.stopBurnLeaderRanksRefresh();
      this.burnLeaderRanksError = null;
      this.selectedBurnLeaderBlock = null;
    },

    stopBurnLeaderRanksRefresh() {
      if (this.burnLeaderRanksRefreshTimer === null) return;
      clearTimeout(this.burnLeaderRanksRefreshTimer);
      this.burnLeaderRanksRefreshTimer = null;
    },

    scheduleBurnLeaderRanksRefresh(delay) {
      this.stopBurnLeaderRanksRefresh();
      if (
        !this.burnLeaderRanksPending(this.selectedBurnLeaderBlock) ||
        this.burnLeaderRanksError
      ) return;
      this.burnLeaderRanksRefreshTimer = setTimeout(() => {
        this.burnLeaderRanksRefreshTimer = null;
        this.refreshSelectedBurnLeaderRanks();
      }, delay);
    },

    async refreshSelectedBurnLeaderRanks() {
      const selected = this.selectedBurnLeaderBlock;
      if (
        !this.burnLeaderRanksPending(selected) ||
        this.burnLeaderRanksError ||
        this.burnLeaderRanksRefreshPromise
      ) return;
      const expectedHash = selected.hash;
      const beforeHeight = Number(selected.height) + 1;
      if (!Number.isSafeInteger(beforeHeight) || beforeHeight <= 0) {
        this.burnLeaderRanksError = "Unable to refresh burn leader ranks for this block.";
        return;
      }
      this.burnLeaderRanksRefreshPromise = this.fetchJson(
        `/api/blocks?before_height=${beforeHeight}&limit=1`
      );
      try {
        const blocks = await this.burnLeaderRanksRefreshPromise;
        if (this.selectedBurnLeaderBlock?.hash !== expectedHash) return;
        const refreshed = blocks.find((block) => block.hash === expectedHash);
        if (!refreshed) {
          this.burnLeaderRanksError = "This block is no longer on the active chain.";
          return;
        }
        this.blocks = this.blocks.map((block) => block.hash === expectedHash ? refreshed : block);
        if (this.selectedBlock?.hash === expectedHash) this.selectedBlock = refreshed;
        this.selectedBurnLeaderBlock = refreshed;
        this.burnLeaderRanksError = null;
      } catch (error) {
        if (this.selectedBurnLeaderBlock?.hash === expectedHash) {
          this.burnLeaderRanksError = error.message || "Failed to load burn leader ranks.";
        }
      } finally {
        this.burnLeaderRanksRefreshPromise = null;
        if (
          this.burnLeaderRanksPending(this.selectedBurnLeaderBlock) &&
          !this.burnLeaderRanksError
        ) {
          this.scheduleBurnLeaderRanksRefresh(this.syncingNode() ? 1000 : 5000);
        }
      }
    },

    openBlockBytesModal(block) {
      this.selectedByteBlock = block;
    },

    closeBlockBytesModal() {
      this.selectedByteBlock = null;
    },

    openBurnBundleModal(block) {
      this.selectedBurnBundleBlock = block;
    },

    closeBurnBundleModal() {
      this.selectedBurnBundleBlock = null;
    },

    openTransactionModal(tx, context = {}) {
      this.selectedTransaction = { tx, context };
    },

    openBlockRewardModal(block) {
      this.openTransactionModal(this.blockRewardTransaction(block), {
        source: "Block reward",
        blockHeight: block.height,
        blockFinalizer: block.miner,
        reward: true,
      });
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

    openWalletAddressesModal() {
      this.showWalletAddresses = true;
    },

    closeWalletAddressesModal() {
      this.showWalletAddresses = false;
    },

    openPowDifficultyInfo() {
      this.showPowDifficultyInfo = true;
    },

    closePowDifficultyInfo() {
      this.showPowDifficultyInfo = false;
    },

    openChainResetModal() {
      this.chainResetConfirm = "";
      this.chainResetModalOpen = true;
    },

    closeChainResetModal() {
      if (this.chainResetBusy) return;
      this.chainResetModalOpen = false;
      this.chainResetConfirm = "";
    },

    async resetLocalChain() {
      if (this.chainResetConfirm.trim() !== "RESET") {
        this.showFlash("Type RESET to confirm deleting the local chain", "error");
        return;
      }
      this.chainResetBusy = true;
      try {
        await this.submitForm("/api/settings/chain-reset", {
          confirm: this.chainResetConfirm,
        });
        this.blocks = [];
        this.selectedBlock = null;
        this.selectedByteBlock = null;
        this.selectedBurnLeaderBlock = null;
        this.selectedTransaction = null;
        this.mempool = [];
        this.walletTxs = [];
        this.walletUtxos = [];
        this.mempoolFirstSeenHeights = {};
        this.mempoolFirstSeenAt = {};
        this.mempoolSeenInitialized = false;
        this.lastBlockMempoolHeight = null;
        this.resetPageState("walletTx");
        this.resetPageState("walletUtxo");
        this.resetPageState("mempool");
        this.chainResetModalOpen = false;
        this.chainResetConfirm = "";
        await this.refresh({ force: true });
        this.showFlash("Local chain deleted. Sync requested from peers.", "success");
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        this.chainResetBusy = false;
      }
    },

    closeModals() {
      this.closeDesktopUpdateModal();
      this.closeOptimizeWallet();
      this.closeSendConfirmModal();
      this.closeTransactionModal();
      this.closeWalletUtxosModal();
      this.closeWalletAddressesModal();
      this.closePowDifficultyInfo();
      this.closeBurnLeaderRanksModal();
      this.closeBurnBundleModal();
      this.closeChainResetModal();
      this.closePeerModal();
    },

    async loadOlderBlocks() {
      if (!this.canUseProtectedApi()) return;
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
      await this.refresh({ force: true });
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
      const response = await this.fetchWithTimeout(path, {
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
      if (this.tab === "wallet") {
        await this.refreshTransferFeeEstimate();
        return;
      }
      if (this.tab === "mining") {
        await Promise.all([
          this.refreshBurnFeeEstimate(),
          this.refreshMineFeeEstimate(),
        ]);
      }
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
        utxos: this.hybridTransferRecipient() ? this.selectedTransferUtxos.join("\n") : "",
      });
    },

    async fetchFeeEstimate(path, fields) {
      try {
        const body = new URLSearchParams();
        for (const [key, value] of Object.entries(fields)) body.set(key, value);
        const response = await this.fetchWithTimeout(path, {
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

    feeEstimateError(kind) {
      return Boolean(this.feeEstimates[kind]?.error);
    },

    feeExceedsAmount(kind) {
      const estimate = this.feeEstimates[kind];
      if (!estimate || estimate.error) return false;
      const amount = kind === "burn"
        ? this.parseiunaAmount(this.burnAmountDraft)
        : this.parseiunaAmount(this.transferAmount);
      const fee = Number(estimate.fee);
      return amount > 0 && Number.isFinite(fee) && fee > amount;
    },

    feeExceedsAmountLabel(kind) {
      if (!this.feeExceedsAmount(kind)) return "";
      const label = kind === "burn" ? "burn" : "transfer";
      const amount = kind === "burn"
        ? this.parseiunaAmount(this.burnAmountDraft)
        : this.parseiunaAmount(this.transferAmount);
      const fee = Number(this.feeEstimates[kind].fee);
      return `Warning: estimated fee IUNA ${this.amountLabel(fee)} exceeds the ${label} amount IUNA ${this.amountLabel(amount)}.`;
    },

    // Rank 1 may publish from the parent timestamp plus two target block times. A slower rank 0
    // finalizer loses the block and its other eligible tickets, while committee seats come from
    // mined-coin lineage and accept any ticket, however small.
    vdfSpeedHint() {
      const mining = this.status.mining;
      const estimateMs = mining?.estimated_vdf_ms;
      const targetMs = mining?.vdf_target_block_ms;
      if (typeof estimateMs !== "number" || !targetMs) return null;
      const fallbackSlotMs = 2 * targetMs;
      if (estimateMs < 0.75 * fallbackSlotMs) return null;
      const tooSlow = estimateMs >= fallbackSlotMs;
      const source = mining.vdf_speed_source === "finalization" ? "Your last VDF run" : "A quick benchmark";
      const timing = `${source} suggests this machine needs about ${this.durationLabel(estimateMs)} for a block VDF; a fallback finalizer can take over after ${this.durationLabel(fallbackSlotMs)}.`;
      const risk = tooSlow
        ? "If your burn wins, the block will most likely go to a fallback and your eligible tickets are invalidated."
        : "On a busy moment you could miss your slot and lose your eligible tickets.";
      return {
        level: tooSlow ? "warning" : "notice",
        text: `${timing} ${risk} Keep your burn low: committee seats depend on mined coins, not burn size.`,
        canLower: this.burnAmount > IUNA_LOW_BURN_AMOUNT,
      };
    },

    async useLowBurnAmount() {
      this.burnAmountDraft = this.amountLabel(IUNA_LOW_BURN_AMOUNT);
      this.burnAmountDirty = true;
      await this.saveBurn();
      this.scheduleFeeEstimates();
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
        this.appendMiningEvent("Burn settings saved", `Configured ${this.amountLabel(amount)} IUNA per block with ${this.amountLabel(fee)} IUNA fee/byte.`, "info");
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
        this.appendMiningEvent(
          enabled ? "Finalization burns turned on" : "Finalization burns turned off",
          enabled
            ? `Burning ${this.amountLabel(amount)} IUNA per block with ${this.amountLabel(fee)} IUNA fee/byte.`
            : "Automatic burn preparation paused.",
          enabled ? "active" : "warning"
        );
        this.miningEventState.pob = enabled ? "on" : "off";
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
          { enabled, workers: this.powMiningWorkers },
          enabled ? "PoW mining turned on" : "PoW mining turned off"
        );
        this.appendMiningEvent(
          enabled ? "PoW mining turned on" : "PoW mining turned off",
          enabled
            ? `Resource budget: ${this.powMiningWorkers} worker${this.powMiningWorkers === 1 ? "" : "s"}.`
            : "PoW worker search paused.",
          enabled ? "active" : "warning"
        );
        this.miningEventState["pow-workers"] = String(this.powMiningWorkers);
      } catch (error) {
        this.powMiningEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setPowMiningWorkers(workers) {
      const previous = this.powMiningWorkers;
      const parsed = Number.parseInt(workers, 10);
      const clamped = Math.min(
        this.maxPowMiningWorkers,
        Math.max(1, Number.isFinite(parsed) ? parsed : 1)
      );
      try {
        this.powMiningWorkers = clamped;
        await this.postForm(
          "/api/settings/pow-mining",
          { enabled: this.powMiningEnabled, workers: clamped },
          `PoW workers set to ${clamped}`
        );
        this.appendMiningEvent(
          "PoW worker budget changed",
          `Resource budget: ${clamped} worker${clamped === 1 ? "" : "s"}.`,
          "info"
        );
        this.miningEventState["pow-workers"] = String(clamped);
      } catch (error) {
        this.powMiningWorkers = previous;
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
          enabled ? "Development mode is preparing" : "Development mode turned off"
        );
        await this.refreshConfig();
        if (!enabled && (this.tab === "metrics" || this.tab === "leaderboards")) {
          this.setTab("settings");
        }
      } catch (error) {
        this.keepTrackOfMetrics = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setRecoveryVdfTopRankPercent(percent) {
      const previous = this.recoveryVdfTopRankPercent;
      const normalized = Math.max(0, Math.min(100, Math.round(Number(percent) || 0)));
      try {
        this.recoveryVdfTopRankPercent = normalized;
        await this.postForm(
          "/api/settings/recovery-vdf",
          { top_rank_percent: String(normalized) },
          `Fallback VDF threshold set to top ${normalized}%`
        );
        await this.refreshConfig();
      } catch (error) {
        this.recoveryVdfTopRankPercent = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setVdfMemoryMib(memoryMib) {
      const previous = this.vdfMemoryMib;
      const normalized = Math.max(32, Math.min(4096, Math.round(Number(memoryMib) || 256)));
      try {
        this.vdfMemoryMib = normalized;
        await this.postForm(
          "/api/settings/vdf-memory",
          { memory_mib: String(normalized) },
          `VDF memory budget set to ${normalized} MiB; applies to the next VDF`
        );
        await this.refreshConfig();
      } catch (error) {
        this.vdfMemoryMib = previous;
        this.showFlash(error.message, "error");
      }
    },

    async setP2pAcceptInbound(enabled) {
      const previous = this.p2pAcceptInbound;
      const walletWasEnabled = this.walletEndpointEnabled;
      try {
        this.p2pAcceptInbound = enabled;
        await this.postForm(
          "/api/settings/p2p-inbound",
          { enabled, bind_port: this.p2pBindPortValue() },
          enabled ? "Public node setting saved" : "Switched to outbound-only P2P"
        );
        this.p2pBindPortDirty = false;
        await this.refreshConfig();
        if (!enabled && walletWasEnabled && this.walletEndpointRestartRequired()) {
          this.showFlash(this.walletEndpointRestartMessage(), "success");
        }
      } catch (error) {
        this.p2pAcceptInbound = previous;
        this.showFlash(error.message, "error");
      }
    },

    p2pBindPortValue() {
      const port = Number(this.p2pBindPort);
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        throw new Error("P2P bind port must be between 1 and 65535");
      }
      return port;
    },

    p2pConfiguredBindAddr() {
      const port = Number(this.config.p2p_bind_port || 9444);
      if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
      return `0.0.0.0:${port}`;
    },

    p2pRestartRequired() {
      const runtimeActive = this.config.p2p_inbound_runtime_active === true;
      if (this.p2pAcceptInbound !== runtimeActive) return true;
      if (!this.p2pAcceptInbound) return false;
      const configured = this.p2pConfiguredBindAddr();
      return configured ? this.config.p2p_runtime_bind_addr !== configured : false;
    },

    p2pRestartMessage() {
      if (!this.p2pRestartRequired()) return "";
      if (!this.p2pAcceptInbound && this.config.p2p_inbound_runtime_active === true) {
        return "Restart iuna to close the public P2P listener.";
      }
      const configured = this.p2pConfiguredBindAddr();
      return `Restart iuna to open public P2P on ${configured || "the configured bind port"}.`;
    },

    async setStratumEnabled(enabled) {
      const previous = this.stratumEnabled;
      try {
        this.stratumEnabled = enabled;
        await this.postForm(
          "/api/settings/stratum",
          { enabled, bind_port: this.stratumBindPortValue() },
          enabled ? "Stratum endpoint setting saved" : "Stratum endpoint disabled"
        );
        this.stratumBindPortDirty = false;
        await this.refreshConfig();
        const restartMessage = this.stratumRestartMessage();
        if (restartMessage) this.showFlash(restartMessage, "success");
      } catch (error) {
        this.stratumEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    stratumBindPortValue() {
      const port = Number(this.stratumBindPort);
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        throw new Error("Stratum bind port must be between 1 and 65535");
      }
      return port;
    },

    stratumConfiguredBindAddr() {
      const port = Number(this.config.stratum_bind_port || 3333);
      if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
      return `0.0.0.0:${port}`;
    },

    stratumRestartRequired() {
      const runtimeActive = this.config.stratum_runtime_enabled === true;
      if (this.stratumEnabled !== runtimeActive) return true;
      if (!this.stratumEnabled) return false;
      const configured = this.stratumConfiguredBindAddr();
      return configured ? this.config.stratum_runtime_listen_addr !== configured : false;
    },

    stratumRestartMessage() {
      if (!this.stratumRestartRequired()) return "";
      if (!this.stratumEnabled && this.config.stratum_runtime_enabled === true) {
        return "Restart iuna to close the Stratum listener.";
      }
      const configured = this.stratumConfiguredBindAddr();
      return `Restart iuna to open Stratum on ${configured || "the configured bind port"}.`;
    },

    async saveStratumSettings() {
      try {
        await this.postForm(
          "/api/settings/stratum",
          { enabled: this.stratumEnabled, bind_port: this.stratumBindPortValue() },
          "Stratum endpoint setting saved"
        );
        this.stratumBindPortDirty = false;
        await this.refreshConfig();
        const restartMessage = this.stratumRestartMessage();
        if (restartMessage) this.showFlash(restartMessage, "success");
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async setWalletEndpointEnabled(enabled) {
      if (enabled && !this.p2pAcceptInbound) {
        this.showFlash("Enable Public node before enabling the wallet endpoint", "error");
        return;
      }
      const previous = this.walletEndpointEnabled;
      try {
        this.walletEndpointEnabled = enabled;
        await this.postForm(
          "/api/settings/wallet-endpoint",
          { enabled, bind_port: this.walletEndpointBindPortValue() },
          enabled ? "Wallet endpoint setting saved" : "Wallet endpoint disabled"
        );
        this.walletEndpointBindPortDirty = false;
        await this.refreshConfig();
        const restartMessage = this.walletEndpointRestartMessage();
        if (restartMessage) this.showFlash(restartMessage, "success");
      } catch (error) {
        this.walletEndpointEnabled = previous;
        this.showFlash(error.message, "error");
      }
    },

    walletEndpointBindPortValue() {
      const port = Number(this.walletEndpointBindPort);
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        throw new Error("Wallet endpoint bind port must be between 1 and 65535");
      }
      return port;
    },

    walletEndpointConfiguredBindAddr() {
      const port = Number(this.config.wallet_endpoint_bind_port || 18662);
      if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
      return `0.0.0.0:${port}`;
    },

    walletEndpointRestartRequired() {
      const runtimeActive = this.config.wallet_endpoint_runtime_enabled === true;
      if (this.walletEndpointEnabled !== runtimeActive) return true;
      if (!this.walletEndpointEnabled) return false;
      const configured = this.walletEndpointConfiguredBindAddr();
      return configured
        ? this.config.wallet_endpoint_runtime_listen_addr !== configured
        : false;
    },

    walletEndpointRestartMessage() {
      if (!this.walletEndpointRestartRequired()) return "";
      if (!this.walletEndpointEnabled && this.config.wallet_endpoint_runtime_enabled === true) {
        return "Restart iuna to close the public wallet listener.";
      }
      const configured = this.walletEndpointConfiguredBindAddr();
      return `Restart iuna to open the wallet API on ${configured || "the configured bind port"}.`;
    },

    async saveWalletEndpointSettings() {
      if (!this.p2pAcceptInbound) {
        this.showFlash("Enable Public node before configuring the wallet endpoint", "error");
        return;
      }
      try {
        await this.postForm(
          "/api/settings/wallet-endpoint",
          { enabled: this.walletEndpointEnabled, bind_port: this.walletEndpointBindPortValue() },
          "Wallet endpoint setting saved"
        );
        this.walletEndpointBindPortDirty = false;
        await this.refreshConfig();
        const restartMessage = this.walletEndpointRestartMessage();
        if (restartMessage) this.showFlash(restartMessage, "success");
      } catch (error) {
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
        if (this.p2pBindPortDirty) {
          await this.submitForm("/api/settings/p2p-inbound", {
            enabled: true,
            bind_port: this.p2pBindPortValue(),
          });
          this.p2pBindPortDirty = false;
        }
        await this.postForm(
          "/api/settings/p2p-announce",
          { addr },
          addr ? "P2P announce address saved" : "P2P announce address cleared"
        );
        this.p2pAnnounceAddr = addr;
        this.p2pAnnounceDirty = false;
        await this.refreshConfig();
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

    pobStatusLabel() {
      const mining = this.status.mining;
      if (!mining) return "-";
      if (this.status.wallet_locked) return "Wallet locked";
      if (!mining.automatic) return "Off";
      if ((mining.burn_per_block ?? 0) <= 0) return "Anchor only";
      if (mining.wallet_is_current_leader) return "Selected";
      if (mining.current_leader) return "Waiting";
      return "Recovery standby";
    },

    powStatusShortLabel() {
      if (this.status.wallet_locked) return "Wallet locked";
      if (!this.powMiningEnabled) return "Off";
      const status = this.status.mining?.last_auto_pow_mine_status || "";
      if (status.includes("queued")) return "Queued";
      if (status.includes("searched")) return "Searching";
      if (status.includes("waiting")) return "Waiting";
      if (status.includes("failed")) return "Error";
      return `${this.powMiningWorkers} worker${this.powMiningWorkers === 1 ? "" : "s"}`;
    },

    pobDetailLabel() {
      return this.status.mining?.last_auto_finalization_status || "Waiting for next automatic finalization tick";
    },

    autoPowStatusLabel() {
      if (!this.powMiningEnabled) return "PoW mining is off";
      const status =
        this.status.mining?.last_auto_pow_mine_status || "Waiting for next automatic PoW mining tick";
      return `${status} (${this.powMiningWorkers} worker${this.powMiningWorkers === 1 ? "" : "s"})`;
    },

    currentFinalizerLabel() {
      const leader = this.currentFinalizerAddress();
      if (!leader) return "-";
      if (this.isOwnAddress(leader)) return "you";
      return this.shortAddressLabel(leader);
    },

    currentFinalizerAddress() {
      return this.status.mining?.current_leader ?? this.status.chain?.next_leader ?? null;
    },

    localMiningMempoolLabel() {
      const pending = this.status.chain?.pending_transactions;
      if (typeof pending !== "number") return "-";
      const visibleMines = this.localMineActionCount();
      return `${pending} pending / ${visibleMines} visible mines`;
    },

    powDifficultyLabel() {
      return this.status.chain?.current_mine_difficulty_bits ?? this.status.launch_profile?.mine_difficulty_bits ?? "-";
    },

    localMineActionCount() {
      return this.mempool.filter((tx) => tx?.kind === "mine").length;
    },

    appendMiningEvent(title, detail, kind = "info", timestamp = new Date()) {
      const last = this.miningEvents[0];
      if (last?.title === title && last?.detail === detail && last?.kind === kind) return;
      this.miningEventCounter += 1;
      const entry = {
        key: `${timestamp.getTime()}-${this.miningEventCounter}`,
        timestamp,
        time: timestamp.toLocaleTimeString(),
        kind,
        title,
        detail,
      };
      this.miningEvents = [entry, ...this.miningEvents].slice(0, this.miningEventLimit);
    },

    isPowMineSuccessStatus(status) {
      return /queued mine action/i.test(status || "");
    },

    syncMiningEvents({ status, blocks }) {
      const mining = status?.mining || {};
      const chain = status?.chain || {};
      if (!this.miningEventState.started) {
        this.appendMiningEvent(
          "Mining log started",
          `Height ${chain.height ?? "-"}, PoB ${mining.automatic ? "on" : "off"}, PoW ${mining.pow_mining_enabled ? "on" : "off"}.`,
          "info"
        );
        this.miningEventState.started = true;
      }

      this.noteMiningStateChange(
        "pob",
        mining.automatic ? "on" : "off",
        mining.automatic ? "Finalization burns active" : "Finalization burns inactive",
        mining.automatic
          ? `Burning ${this.amountLabel(mining.burn_per_block || 0)} IUNA per block with ${this.amountLabel(mining.automatic_burn_fee || 0)} IUNA fee/byte.`
          : "Automatic burn preparation is off.",
        mining.automatic ? "active" : "warning"
      );
      const finalizationStatus = mining.last_auto_finalization_status || "";
      this.noteMiningStateChange(
        "pob-status",
        finalizationStatus,
        "PoB status",
        finalizationStatus || "Waiting for next automatic finalization tick.",
        mining.automatic ? "active" : "info"
      );
      this.noteMiningStateChange(
        "pow-workers",
        String(mining.pow_mining_workers ?? this.powMiningWorkers),
        "PoW worker budget",
        `Resource budget: ${mining.pow_mining_workers ?? this.powMiningWorkers} worker${(mining.pow_mining_workers ?? this.powMiningWorkers) === 1 ? "" : "s"}.`,
        "info"
      );
      const powMineStatus = mining.last_auto_pow_mine_status || "";
      if (this.isPowMineSuccessStatus(powMineStatus)) {
        this.noteMiningStateChange(
          "pow-mine-success",
          powMineStatus,
          "You mined a PoW action",
          `${powMineStatus}. Waiting for a finalizer to include it in a block.`,
          "active"
        );
      } else {
        this.noteMiningStateChange(
          "pow-status",
          powMineStatus,
          "PoW status",
          powMineStatus || "Waiting for next automatic PoW mining tick.",
          mining.pow_mining_enabled ? "active" : "info"
        );
      }
      this.noteMiningStateChange(
        "leader",
        mining.current_leader || "",
        mining.wallet_is_current_leader ? "This wallet is selected" : "Selected finalizer changed",
        mining.current_leader
          ? `Current finalizer: ${this.currentFinalizerLabel()} at height ${chain.height ?? "-"}.`
          : `No current finalizer reported at height ${chain.height ?? "-"}.`,
        mining.wallet_is_current_leader ? "active" : "info"
      );
      if (typeof mining.last_auto_burn_height === "number") {
        this.noteMiningStateChange(
          "last-burn-height",
          String(mining.last_auto_burn_height),
          `Automatic burn prepared at height ${mining.last_auto_burn_height}`,
          "Eligible for the next block opportunity.",
          "active"
        );
      }

      const latestBlock = Array.isArray(blocks)
        ? blocks.find((block) => Number(block?.height) > 0)
        : this.blocks.find((block) => Number(block?.height) > 0);
      if (latestBlock) {
        const finalizer = this.addressLabel(latestBlock.miner);
        const locallyFinalized = this.isOwnAddress(latestBlock.miner);
        if (locallyFinalized) {
          this.noteMiningStateChange(
            "latest-local-block",
            latestBlock.hash || String(latestBlock.height),
            `You finalized block ${latestBlock.height}`,
            `Success. ${this.burnCountLabel(latestBlock)} burned, fees IUNA ${this.amountLabel(latestBlock.total_fees ?? latestBlock.totalFees ?? 0)}.`,
            "active",
            new Date(Number(latestBlock.timestamp_ms ?? latestBlock.timestampMs) || Date.now())
          );
        }
        if (!locallyFinalized) {
          this.noteMiningStateChange(
            "latest-block",
            latestBlock.hash || String(latestBlock.height),
            `Observed block ${latestBlock.height}`,
            `Finalized by ${finalizer}. ${this.burnCountLabel(latestBlock)} burned, fees IUNA ${this.amountLabel(latestBlock.total_fees ?? latestBlock.totalFees ?? 0)}.`,
            "active",
            new Date(Number(latestBlock.timestamp_ms ?? latestBlock.timestampMs) || Date.now())
          );
        }
      }
    },

    noteMiningStateChange(key, value, title, detail, kind = "info", timestamp = new Date()) {
      if (this.miningEventState[key] === value) return;
      this.miningEventState[key] = value;
      if (value === "" && key !== "pow-status" && key !== "leader") return;
      this.appendMiningEvent(title, detail, kind, timestamp);
    },

    miningEventLog() {
      return this.miningEvents;
    },

    metricsCharts() {
      return Array.isArray(this.blockchainMetrics?.charts) ? this.blockchainMetrics.charts : [];
    },

    metricsPreparing() {
      return this.blockchainMetrics?.preparing === true;
    },

    metricsLatest() {
      return this.blockchainMetrics?.latest || {};
    },

    metricsLeaderboards() {
      return this.blockchainMetrics?.leaderboards || {};
    },

    topMineProofRows() {
      const rows = this.blockchainMetrics?.topMineProofs;
      return Array.isArray(rows) ? rows : [];
    },

    leaderboardRows(kind) {
      const rows = this.metricsLeaderboards()?.[kind];
      return Array.isArray(rows) ? rows : [];
    },

    leaderboardPlaceholderRanks(rowCount) {
      const count = Math.min(10, Math.max(0, Number(rowCount) || 0));
      return Array.from({ length: 10 - count }, (_, index) => count + index);
    },

    leaderboardTitle(kind) {
      return {
        balances: "Top 10 Balance",
        miners: "Top 10 Miners",
        burners: "Top 10 Burners",
      }[kind] || "Leaderboard";
    },

    leaderboardRankLabel(index) {
      return ["Gold", "Silver", "Bronze"][index] || `#${index + 1}`;
    },

    leaderboardRankClass(index) {
      return index < 3 ? `medal-${index + 1}` : "";
    },

    leaderboardAmountLabel(row) {
      return `IUNA ${this.amountLabel(row?.amount || 0)}`;
    },

    leaderboardCountLabel(kind, row) {
      const count = Number(row?.count || 0);
      if (kind === "miners") return `${count} mine${count === 1 ? "" : "s"}`;
      if (kind === "burners") return `${count} burn${count === 1 ? "" : "s"}`;
      return `${count} UTXO${count === 1 ? "" : "s"}`;
    },

    metricsPath(range = this.metricsRange) {
      return range === "all" ? "/api/metrics" : `/api/metrics?limit=${range}`;
    },

    setMetricsRange(range) {
      this.metricsRange = range === 1000 || range === "all" ? range : 100;
      this.metricHover = null;
      try {
        localStorage.setItem("iunaMetricsRange", String(this.metricsRange));
      } catch {
        // Non-persistent filtering is fine when storage is unavailable.
      }
      if (this.tab === "metrics") {
        this.refreshMetrics();
      }
    },

    async fetchMetricsResponse(range = this.metricsRange) {
      return this.prepareMetricsResponse(await this.fetchJson(this.metricsPath(range)));
    },

    async refreshMetrics(options = {}) {
      if (!this.canUseProtectedApi()) return this.blockchainMetrics;
      const requestId = ++this.metricsRequestSeq;
      const range = this.metricsRange;
      if (this.metricsCharts().length === 0 && options.silent !== true) {
        this.loadingMetrics = true;
      }
      try {
        const metrics = await this.fetchMetricsResponse(range);
        if (requestId === this.metricsRequestSeq && this.metricsRange === range) {
          this.blockchainMetrics = metrics;
        }
        return metrics;
      } catch (error) {
        if (options.silent !== true && !error.uiDataLoading) this.showFlash(error.message, "error");
        return this.blockchainMetrics;
      } finally {
        if (requestId === this.metricsRequestSeq) {
          this.loadingMetrics = false;
        }
      }
    },

    prepareMetricsResponse(metrics) {
      const charts = Array.isArray(metrics?.charts)
        ? metrics.charts.map((chart) => this.prepareMetricChart(chart))
        : [];
      return { ...(metrics || {}), charts };
    },

    prepareMetricChart(chart) {
      const points = this.metricValidPoints(chart);
      const bounds = this.metricChartBoundsForPoints(points);
      const yTicks = this.metricYAxisTicksForPoints(points);
      const xTicks = this.metricXAxisTicksForPoints(points);
      const linePoints = points
        .map((point) => {
          const x = this.metricXAxisPositionFromBounds(bounds, Number(point.height));
          const y = this.metricYAxisPositionFromBounds(bounds, Number(point.value));
          return `${x.toFixed(1)},${y.toFixed(1)}`;
        })
        .join(" ");
      const markers = points.map((point) => {
        const height = Number(point.height);
        const value = Number(point.value);
        return {
          height,
          value,
          x: this.metricXAxisPositionFromBounds(bounds, height),
          y: this.metricYAxisPositionFromBounds(bounds, value),
        };
      });
      const gridPath = [
        ...yTicks.map((tick) => {
          const y = this.metricYAxisPositionFromBounds(bounds, Number(tick)).toFixed(1);
          return `M4 ${y} H296`;
        }),
        ...xTicks.map((tick) => {
          const x = this.metricXAxisPositionFromBounds(bounds, Number(tick)).toFixed(1);
          return `M${x} 8 V132`;
        }),
      ].join(" ");
      return {
        ...chart,
        _visiblePoints: points,
        _bounds: bounds,
        _yTicks: yTicks,
        _xTicks: xTicks,
        _linePoints: linePoints,
        _markers: markers,
        _gridPath: gridPath,
      };
    },

    metricChartPoints(chart) {
      return chart?._linePoints || "";
    },

    metricChartPointMarkers(chart) {
      return chart?._markers || [];
    },

    metricGridPath(chart) {
      return chart?._gridPath || "";
    },

    metricValidPoints(chart) {
      const points = Array.isArray(chart?.points) ? chart.points : [];
      return points.filter((point) => Number.isFinite(Number(point.value)));
    },

    metricVisiblePoints(chart) {
      return chart?._visiblePoints || this.metricValidPoints(chart);
    },

    metricLatestValueLabel(chart) {
      const points = this.metricVisiblePoints(chart);
      if (points.length === 0) return "-";
      return this.metricValueLabel(chart, points[points.length - 1].value);
    },

    metricChartBounds(chart) {
      return chart?._bounds || this.metricChartBoundsForPoints(this.metricVisiblePoints(chart));
    },

    metricChartBoundsForPoints(points) {
      if (points.length === 0) {
        return { minHeight: 0, maxHeight: 1, minValue: 0, maxValue: 1 };
      }
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
      return chart?._yTicks || this.metricYAxisTicksForPoints(this.metricVisiblePoints(chart));
    },

    metricYAxisTicksForPoints(points) {
      if (points.length === 0) return [];
      const values = points.map((point) => Number(point.value));
      return this.niceTicks(Math.min(...values), Math.max(...values), 5).reverse();
    },

    metricXAxisTicks(chart) {
      return chart?._xTicks || this.metricXAxisTicksForPoints(this.metricVisiblePoints(chart));
    },

    metricXAxisTicksForPoints(points) {
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

    metricHoverPointStyle(chart) {
      const hover = this.metricHover;
      if (!hover || hover.chartId !== chart.id) return "";
      return `left: ${(hover.x / 300) * 100}%; top: ${(hover.y / 148) * 100}%;`;
    },

    setMetricHover(chart, marker) {
      this.metricHover = {
        chartId: chart.id,
        height: marker.height,
        value: marker.value,
        x: marker.x,
        y: marker.y,
        label: this.metricPointLabel(chart, marker),
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
      if (chart?.valueKind === "bytes") return `${this.compactNumber(number)} B`;
      return this.compactNumber(number);
    },

    metricValueLabel(chart, value) {
      const number = Number(value);
      if (!Number.isFinite(number)) return "-";
      if (chart?.valueKind === "iuna") return `IUNA ${this.compactNumber(number)}`;
      if (chart?.valueKind === "seconds") return `${this.compactNumber(number)} s`;
      if (chart?.valueKind === "bytes") return `${this.compactNumber(number)} bytes`;
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
      const microiuna = this.microiunaAmount(value);
      const whole = Math.floor(microiuna / 1000000);
      const fractional = String(microiuna % 1000000).padStart(6, "0").replace(/0+$/, "");
      return fractional ? `${whole}.${fractional}` : `${whole}`;
    },

    microiunaAmount(value) {
      return Math.max(0, Math.round(Number(value) || 0));
    },

    metricAmountLabel(value) {
      return value === null || value === undefined ? "-" : `IUNA ${this.amountLabel(value)}`;
    },

    amountNumber(value) {
      return Number(this.amountLabel(value));
    },

    parseiunaAmount(value) {
      const text = String(value ?? "").trim().replace(",", ".");
      if (!text) return 0;
      const match = text.match(/^(\d+)(?:\.(\d{0,6})\d*)?$/);
      if (!match) return 0;
      const whole = Number(match[1] || 0);
      const fractional = Number((match[2] || "").padEnd(6, "0"));
      return Math.max(0, Math.trunc(whole * 1000000 + fractional));
    },

    parseiunaAmountRequired(value, message) {
      const text = String(value ?? "").trim().replace(",", ".");
      if (!text) throw new Error(message);
      const parsed = this.parseiunaAmount(text);
      if (parsed === 0 && !/^0(?:\.0*)?$/.test(text)) throw new Error(message);
      return parsed;
    },

    showOptimizeSuggestion() {
      if (this.optimizeDismissed || this.walletSpendableUtxoCount() < 500) return false;
      try {
        return Date.now() > Number(localStorage.getItem(`iunaOptimizeLater:${this.status.wallet_address}`) || 0);
      } catch { return true; }
    },

    walletSpendableUtxoCount() {
      const funded = this.fundedWalletAddresses();
      if (funded.length > 0 && funded.every((entry) => Number.isFinite(Number(entry?.spendable_utxos)))) {
        return funded.reduce((total, entry) => total + Number(entry.spendable_utxos), 0);
      }
      return Number(this.walletUtxoPage.total || 0);
    },

    dismissOptimizeSuggestion() {
      this.optimizeDismissed = true;
      try { localStorage.setItem(`iunaOptimizeLater:${this.status.wallet_address}`, String(Date.now() + 7 * 86400000)); } catch {}
    },

    openOptimizeWallet() {
      this.showWalletUtxos = false;
      this.optimizeOpen = true;
    },

    closeOptimizeWallet() {
      if (this.optimizeRunning) return;
      this.optimizeOpen = false;
    },

    async previewOptimization() {
      if (this.optimizeBusy || this.optimizeRunning) return;
      this.optimizeBusy = true;
      this.optimizePlan = null;
      this.optimizeError = "";
      this.optimizeMessage = "";
      try {
        const rate = this.parseiunaAmountRequired(this.optimizeFee, "Enter a fee per byte");
        if (!Number.isSafeInteger(rate) || rate < 1) throw new Error("Fee per byte must be at least 0.000001 IUNA");
        const result = await this.submitForm("/api/wallet/optimize/preview", {
          fee_per_byte: rate, merge_roots: this.optimizeMergeRoots,
        });
        const plan = result.plan;
        if (![plan.fee, ...plan.batches.flatMap(batch => [batch.fee, batch.amount])].every(Number.isSafeInteger)) {
          throw new Error("Amounts exceed the safe range for this interface");
        }
        this.optimizePlan = { ...plan, rate, mergeRoots: this.optimizeMergeRoots };
      } catch (error) { this.optimizeError = error.message; }
      finally { this.optimizeBusy = false; }
    },

    async previewQuantumMigration() {
      if (this.quantumMigrationBusy) return;
      this.quantumMigrationBusy = true;
      this.quantumMigrationPreview = null;
      this.quantumMigrationError = "";
      try {
        const rate = this.parseiunaAmountRequired(
          this.quantumMigrationFee,
          "Enter a migration fee per byte"
        );
        if (!Number.isSafeInteger(rate) || rate < 1) {
          throw new Error("Fee per byte must be at least 0.000001 IUNA");
        }
        const result = await this.submitForm("/api/wallet/quantum-migration/preview", {
          fee_per_byte: rate,
        });
        const preview = result.preview;
        if (![preview.fee, preview.amount, preview.bytes, preview.input_count, preview.remaining_legacy_utxos].every(Number.isSafeInteger)) {
          throw new Error("Migration values exceed the safe range for this interface");
        }
        this.quantumMigrationPreview = { ...preview, rate };
      } catch (error) {
        this.quantumMigrationError = error.message;
      } finally {
        this.quantumMigrationBusy = false;
      }
    },

    async submitQuantumMigration() {
      const preview = this.quantumMigrationPreview;
      if (!preview || this.quantumMigrationSubmitting || this.quantumMigrationBusy) return;
      this.quantumMigrationSubmitting = true;
      this.quantumMigrationError = "";
      try {
        let result;
        try {
          result = await this.submitForm("/api/wallet/quantum-migration/submit", {
            fee_per_byte: preview.rate,
            max_fee: preview.fee,
            transaction_id: preview.transaction_id,
          });
        } catch (error) {
          this.quantumMigrationPreview = null;
          throw new Error(`${error.message}. Check wallet activity before requesting a new preview.`);
        }
        this.quantumMigrationPreview = null;
        if (result.persistence_error) {
          throw new Error("Migration queued locally, but durable recovery failed. Keep this node running and check its storage before continuing.");
        }
        if (result.broadcast_error) {
          throw new Error("Migration saved locally, but broadcasting failed. It will be rebroadcast automatically; check connectivity before continuing.");
        }
        const remainder = Number(result.remaining_legacy_utxos || 0);
        this.showFlash(
          remainder > 0
            ? `Migration batch queued. ${remainder} legacy UTXOs remain after confirmation.`
            : "Wallet migration queued.",
          "success"
        );
        await this.refresh({ force: true });
      } catch (error) {
        this.quantumMigrationError = error.message;
      } finally {
        this.quantumMigrationSubmitting = false;
      }
    },

    async runOptimization() {
      const plan = this.optimizePlan;
      if (!plan || this.optimizeRunning || this.optimizeBusy) return;
      this.optimizeRunning = true;
      this.optimizeError = "";
      try {
        for (let batchIndex = 0; batchIndex < plan.batches.length; batchIndex += 1) {
          if (this.status.wallet_locked || this.status.wallet_address !== plan.address) {
            throw new Error("Wallet locked or changed. Unlock the original wallet and request a new preview.");
          }
          const batch = plan.batches[batchIndex];
          this.optimizeMessage = `Submitting batch ${batchIndex + 1} of ${plan.batches.length}…`;
          let result;
          try {
            result = await this.submitForm("/api/wallet/optimize/submit", {
              address: plan.address, fee_per_byte: plan.rate, max_fee: batch.fee,
              merge_roots: plan.mergeRoots, utxos: JSON.stringify(batch.utxos),
            });
            if (!result.signature) throw new Error("No transaction confirmation received from the node");
          } catch (error) {
            // A timeout may mean the transaction was accepted. Never retry it automatically.
            this.optimizePlan = null;
            throw new Error(`${error.message}. Check wallet activity before requesting a new preview.`);
          }
          if (result.broadcast_error) {
            this.optimizePlan = null;
            throw new Error("Batch queued locally, but broadcasting failed. Check connectivity before requesting a new preview.");
          }
        }
        this.optimizeMessage = `Optimization submitted. Expected after confirmation: ${plan.before - plan.after} fewer balance parts; network fees ${this.amountLabel(plan.fee)} IUNA.`;
        this.optimizePlan = null;
        this.optimizeOpen = false;
        this.showFlash(this.optimizeMessage, "success");
        await this.refresh({ force: true });
      } catch (error) {
        this.optimizePlan = null;
        this.optimizeError = error.message;
      }
      finally { this.optimizeRunning = false; }
    },

    async sendTransfer() {
      if (this.sendPreparing || this.sendConfirmBusy) return;
      this.sendPreparing = true;
      try {
        const amount = this.parseiunaAmountRequired(this.transferAmount, "Transfer amount is required");
        if (amount <= 0) throw new Error("Transfer amount must be greater than zero");
        const fee = this.parseiunaAmountRequired(this.transferFee, "Transfer fee per byte is required");
        const fullRecipient = this.transferTo.trim();
        if (!fullRecipient) throw new Error("Recipient is required");
        const estimate = await this.transferFeeEstimateForAmount(amount);
        this.feeEstimates.transfer = estimate;
        this.pendingTransfer = {
          recipient: fullRecipient,
          amount,
          feePerByte: fee,
          bytes: Number(estimate.bytes),
          fee: this.microiunaAmount(estimate.fee),
          utxos: this.hybridTransferRecipient() ? this.selectedTransferUtxos.join("\n") : "",
        };
        this.sendConfirmModalOpen = true;
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        this.sendPreparing = false;
      }
    },

    closeSendConfirmModal() {
      if (this.sendConfirmBusy) return;
      this.sendConfirmModalOpen = false;
      this.pendingTransfer = null;
    },

    async confirmTransfer() {
      const transfer = this.pendingTransfer;
      if (!transfer || this.sendConfirmBusy) return;
      this.sendConfirmBusy = true;
      try {
        const recipient = this.short(transfer.recipient);
        await this.postForm(
          "/api/transfer",
          {
            to: transfer.recipient,
            amount: transfer.amount,
            fee_per_byte: transfer.feePerByte,
            utxos: transfer.utxos,
          },
          `Queued transfer of ${this.amountLabel(transfer.amount)} IUNA to ${recipient}`
        );
        this.sendConfirmModalOpen = false;
        this.pendingTransfer = null;
        this.transferTo = "";
        this.transferAmount = null;
        this.selectedTransferUtxos = [];
        this.selectedTransferUtxoAmounts = {};
        this.showSendAdvanced = false;
        this.feeEstimates.transfer = null;
      } catch (error) {
        this.showFlash(error.message, "error");
      } finally {
        this.sendConfirmBusy = false;
      }
    },

    transferMaxDisabled() {
      if (this.hybridTransferRecipient()) {
        return this.selectedTransferUtxoTotal() <= 0 && Number(this.status.quantum_migration?.hybrid_balance || 0) <= 0;
      }
      return Number(this.status.quantum_migration?.legacy_balance || 0) <= 0;
    },

    hybridTransferRecipient() {
      return !this.isLegacyAddress(this.transferTo);
    },

    isLegacyAddress(address) {
      return !/^(?:iuna|tiuna)1p/i.test(String(address ?? "").trim());
    },

    // Transaction v2 outputs must use address v1, so legacy recipients can only be paid from
    // legacy outputs. Flag the case where all value already sits at hybrid addresses.
    legacyRecipientNeedsHybridAddress() {
      const migration = this.status.quantum_migration || {};
      return this.transferTo.trim() !== ""
        && !this.hybridTransferRecipient()
        && Number(migration.legacy_balance || 0) <= 0
        && Number(migration.hybrid_balance || 0) > 0;
    },

    hybridTransferUtxos() {
      return this.walletUtxos.filter((utxo) => !this.isLegacyAddress(utxo.address));
    },

    transferRecipientChanged() {
      if (!this.hybridTransferRecipient()) {
        this.showSendAdvanced = false;
        this.selectedTransferUtxos = [];
        this.selectedTransferUtxoAmounts = {};
      }
      this.scheduleFeeEstimates();
    },

    async setMaxTransferAmount() {
      try {
        let selectedTotal = this.hybridTransferRecipient()
          ? this.selectedTransferUtxoTotal()
          : Number(this.status.quantum_migration?.legacy_balance || 0);
        if (this.hybridTransferRecipient() && this.selectedTransferUtxos.length === 0) {
          const utxos = await this.fetchJson("/api/wallet/utxos/selectable");
          this.rememberUtxoAmounts(utxos);
          this.selectedTransferUtxos = utxos.map((utxo) => this.utxoOutpoint(utxo));
          this.lastSelectedTransferUtxo =
            this.selectedTransferUtxos[this.selectedTransferUtxos.length - 1] || null;
          selectedTotal = utxos.reduce((sum, utxo) => sum + Number(utxo.amount || 0), 0);
        }

        if (selectedTotal <= 0) {
          this.showFlash("No spendable UTXOs", "error");
          return;
        }

        const amount = await this.maxTransferAmountForSelectedUtxos(selectedTotal);
        this.transferAmount = this.amountLabel(amount);
        this.scheduleFeeEstimates();
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    async maxTransferAmountForSelectedUtxos(selectedTotal) {
      const total = this.microiunaAmount(selectedTotal);
      let low = 1;
      let high = total;
      let bestAmount = 0;
      let bestEstimate = null;

      while (low <= high) {
        const amount = Math.floor((low + high) / 2);
        try {
          const estimate = await this.transferFeeEstimateForAmount(amount);
          const required = amount + this.microiunaAmount(estimate.fee);
          if (required <= total) {
            bestAmount = amount;
            bestEstimate = estimate;
            low = amount + 1;
          } else {
            high = amount - 1;
          }
        } catch {
          high = amount - 1;
        }
      }

      if (bestAmount <= 0 || !bestEstimate) {
        throw new Error("Selected UTXOs do not cover amount plus fee");
      }
      this.feeEstimates.transfer = bestEstimate;
      return bestAmount;
    },

    async transferFeeEstimateForAmount(amount) {
      const recipient = this.transferTo.trim() || this.status.wallet_address;
      if (!recipient) {
        throw new Error("Recipient is required before max can estimate fees");
      }
      const estimate = await this.fetchFeeEstimate("/api/fee-estimate/transfer", {
        to: recipient,
        amount,
        fee_per_byte: this.parseiunaAmount(this.transferFee),
        utxos: this.hybridTransferRecipient() ? this.selectedTransferUtxos.join("\n") : "",
      });
      if (
        estimate?.error
        || !Number.isFinite(Number(estimate?.fee))
        || !Number.isInteger(Number(estimate?.bytes))
        || Number(estimate.bytes) <= 0
      ) {
        throw new Error(estimate?.error || "Could not estimate transfer fee");
      }
      return estimate;
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
        if (this.selectedPeerAddress === peer.address) this.closePeerModal();
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    openPeerModal(peer) {
      this.selectedPeerAddress = peer?.address || null;
    },

    closePeerModal() {
      this.selectedPeerAddress = null;
    },

    peerDetail() {
      return this.peers.find((peer) => peer.address === this.selectedPeerAddress) || null;
    },

    peerCapabilities(peer) {
      return Array.isArray(peer?.last_hello?.capabilities) ? peer.last_hello.capabilities : [];
    },

    peerTimestampLabel(timestampMs) {
      if (typeof timestampMs !== "number" || !Number.isFinite(timestampMs) || timestampMs <= 0) return "-";
      return `${new Date(timestampMs).toLocaleString()} (${this.relativeTimeLabel(timestampMs)})`;
    },

    addressBookEntries() {
      return Object.entries(this.addressBook || {})
        .map(([address, name]) => ({ address, name }))
        .sort((left, right) => left.name.localeCompare(right.name) || left.address.localeCompare(right.address));
    },

    validAddressBookAddress(address) {
      const text = String(address ?? "").trim();
      const hasLower = /[a-z]/.test(text);
      const hasUpper = /[A-Z]/.test(text);
      if (hasLower && hasUpper) return false;
      const normalized = text.toLowerCase();
      const prefix = this.setupAddress().startsWith("tiuna1") ? "tiuna1" : "iuna1";
      return normalized.startsWith(prefix) && /^[02-9ac-hj-np-z]{59}$/.test(normalized.slice(prefix.length));
    },

    selectTransferContact(address) {
      if (!address) return;
      this.transferTo = address;
      this.scheduleFeeEstimates();
      this.closeAddressBookPicker();
    },

    openAddressBookModal(entry = null, standalone = true) {
      this.addressBookEditingAddress = entry?.address || null;
      this.addressBookDraftAddress = entry?.address || "";
      this.addressBookDraftName = entry?.name || "";
      this.addressBookStandalone = standalone;
      if (standalone) this.addressBookOverview = false;
      this.addressBookPickerOpen = true;
      this.addressBookModalOpen = true;
    },

    openAddressBookOverview() {
      this.addressBookEditingAddress = null;
      this.addressBookDraftAddress = "";
      this.addressBookDraftName = "";
      this.addressBookStandalone = false;
      this.addressBookOverview = true;
      this.addressBookModalOpen = false;
      this.addressBookPickerOpen = true;
    },

    openAddressContact(address) {
      const value = String(address ?? "").trim();
      const publicAddress = this.contactAddress(value);
      if (!publicAddress) return;
      const canonical = this.canonicalAddressKey(value);
      const entry = this.addressBookEntries().find(
        (candidate) => this.canonicalAddressKey(candidate.address) === canonical,
      );
      this.addressBookEditingAddress = entry?.address || null;
      this.addressBookDraftAddress = entry && this.validAddressBookAddress(entry.address)
        ? entry.address
        : publicAddress;
      this.addressBookDraftName = entry?.name || "";
      this.addressBookStandalone = true;
      this.addressBookOverview = false;
      this.addressBookPickerOpen = true;
      this.addressBookModalOpen = true;
    },

    contactAddress(address) {
      const normalized = String(address ?? "").trim().toLowerCase();
      if (this.validAddressBookAddress(normalized)) return normalized;
      if (!/^[0-9a-f]{64}$/.test(normalized)) return null;

      const hrp = this.setupAddress().startsWith("tiuna1") ? "tiuna" : "iuna";
      const charset = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
      const bytes = normalized.match(/../g).map((pair) => Number.parseInt(pair, 16));
      const data = [0];
      let accumulator = 0;
      let bits = 0;
      for (const byte of bytes) {
        accumulator = ((accumulator << 8) | byte) & 0xfff;
        bits += 8;
        while (bits >= 5) {
          bits -= 5;
          data.push((accumulator >> bits) & 31);
        }
      }
      if (bits > 0) data.push((accumulator << (5 - bits)) & 31);

      const values = [
        ...[...hrp].map((character) => character.charCodeAt(0) >> 5),
        0,
        ...[...hrp].map((character) => character.charCodeAt(0) & 31),
        ...data,
        0, 0, 0, 0, 0, 0,
      ];
      const generators = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
      let polymod = 1;
      for (const value of values) {
        const top = polymod >>> 25;
        polymod = (((polymod & 0x1ffffff) << 5) ^ value) >>> 0;
        for (let index = 0; index < generators.length; index += 1) {
          if ((top >>> index) & 1) polymod = (polymod ^ generators[index]) >>> 0;
        }
      }
      polymod = (polymod ^ 0x2bc830a3) >>> 0;
      const checksum = Array.from({ length: 6 }, (_, index) => (polymod >>> (5 * (5 - index))) & 31);
      return `${hrp}1${[...data, ...checksum].map((value) => charset[value]).join("")}`;
    },

    hasWalletAddress(address) {
      return this.contactAddress(address) !== null;
    },

    closeAddressBookModal() {
      const closePicker = this.addressBookStandalone;
      this.addressBookModalOpen = false;
      this.addressBookStandalone = false;
      this.addressBookEditingAddress = null;
      this.addressBookDraftAddress = "";
      this.addressBookDraftName = "";
      if (closePicker) this.addressBookPickerOpen = false;
    },

    openAddressBookPicker() {
      this.addressBookStandalone = false;
      this.addressBookOverview = false;
      this.addressBookPickerOpen = true;
    },

    closeAddressBookPicker() {
      this.addressBookPickerOpen = false;
      this.addressBookOverview = false;
      this.closeAddressBookModal();
    },

    async saveAddressBookEntry() {
      const address = this.addressBookDraftAddress.trim();
      const name = this.addressBookDraftName.trim();
      if (!address || !name) {
        this.showFlash("Address and name are required", "error");
        return;
      }
      if (!this.validAddressBookAddress(address)) {
        this.showFlash("Address must be a Bech32m address for this network", "error");
        return;
      }
      const canonicalAddress = address.toLowerCase();
      const oldAddress = this.addressBookEditingAddress;
      if (this.addressBook?.[canonicalAddress] && canonicalAddress !== oldAddress) {
        this.showFlash("Address is already saved", "error");
        return;
      }
      try {
        const fields = oldAddress ? { address, name, old_address: oldAddress } : { address, name };
        await this.submitForm("/api/address-book", fields);
        this.addressBookVersion += 1;
        const nextBook = { ...(this.addressBook || {}) };
        if (oldAddress && oldAddress !== canonicalAddress) delete nextBook[oldAddress];
        nextBook[canonicalAddress] = name;
        this.addressBook = nextBook;
        this.config = { ...this.config, address_book: this.addressBook };
        this.closeAddressBookModal();
        this.showFlash(`Saved ${name}`, "success");
      } catch (error) {
        this.showFlash(error.message, "error");
      }
    },

    editAddressBookEntry(entry) {
      this.openAddressBookModal(entry);
    },

    async removeAddressBookEntry(entry) {
      try {
        await this.submitForm("/api/address-book", { address: entry.address }, "DELETE");
        this.addressBookVersion += 1;
        const nextBook = { ...(this.addressBook || {}) };
        delete nextBook[entry.address];
        this.addressBook = nextBook;
        this.config = { ...this.config, address_book: nextBook };
        if (this.addressBookEditingAddress === entry.address) this.closeAddressBookModal();
        this.showFlash(`Removed ${entry.name}`, "success");
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

    async copyReceiveAddress() {
      try {
        await navigator.clipboard.writeText(this.receiveAddress());
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

    canonicalAddressKey(address) {
      const normalized = String(address ?? "").trim().toLowerCase();
      if (/^[0-9a-f]{64}$/.test(normalized)) return normalized;

      const separator = normalized.lastIndexOf("1");
      const hrp = normalized.slice(0, separator);
      if (separator <= 0 || (hrp !== "iuna" && hrp !== "tiuna")) return normalized;

      const charset = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
      const encoded = normalized.slice(separator + 1);
      if (encoded.length < 7) return normalized;
      const payload = [...encoded.slice(0, -6)].map((character) => charset.indexOf(character));
      if (payload.length === 0 || payload[0] !== 0 || payload.some((value) => value < 0)) return normalized;

      let accumulator = 0;
      let bits = 0;
      const bytes = [];
      for (const value of payload.slice(1)) {
        accumulator = (accumulator << 5) | value;
        bits += 5;
        while (bits >= 8) {
          bits -= 8;
          bytes.push((accumulator >> bits) & 0xff);
          accumulator &= (1 << bits) - 1;
        }
      }
      if (bits >= 5 || accumulator !== 0 || bytes.length !== 32) return normalized;
      return bytes.map((byte) => byte.toString(16).padStart(2, "0")).join("");
    },

    addressName(address) {
      if (!address) return null;
      const normalized = String(address).trim().toLowerCase();
      const directName = this.addressBook?.[normalized];
      if (directName) return directName;

      const canonical = this.canonicalAddressKey(normalized);
      for (const [savedAddress, name] of Object.entries(this.addressBook || {})) {
        if (this.canonicalAddressKey(savedAddress) === canonical) return name;
      }
      return null;
    },

    isOwnAddress(address) {
      if (!address) return false;
      const canonical = this.canonicalAddressKey(address);
      const owned = [
        ...(Array.isArray(this.status.wallet_owned_addresses)
          ? this.status.wallet_owned_addresses
          : []),
        this.status.wallet_address,
        this.status.wallet_receive_address,
        ...this.fundedWalletAddresses().map((entry) => entry.address),
      ];
      return owned.some((candidate) => candidate && this.canonicalAddressKey(candidate) === canonical);
    },

    addressLabel(address) {
      const label = this.addressName(address) || address || "-";
      return this.isOwnAddress(address) ? `${label} (me)` : label;
    },

    shortAddressLabel(address) {
      const label = this.addressName(address) || this.short(address);
      return this.isOwnAddress(address) ? `${label} (me)` : label;
    },

    txFrom(tx) {
      return tx.from ?? tx.inputs?.[0]?.owner ?? "";
    },

    txTo(tx) {
      return tx.to ?? tx.outputs?.[0]?.address ?? null;
    },

    txAmount(tx) {
      if (tx?.kind === "reward" && tx.rewardTotal !== null && tx.rewardTotal !== undefined) {
        return tx.rewardTotal;
      }
      return tx.amount ?? tx.outputs?.[0]?.amount ?? 0;
    },

    isMineTx(tx) {
      return tx?.kind === "mine";
    },

    isRewardTx(tx) {
      return tx?.kind === "reward";
    },

    txFeeLabel(tx) {
      return `IUNA ${this.amountLabel(tx?.fee ?? 0)}`;
    },

    txPillLabel(tx) {
      return tx?.kind || "-";
    },

    txPillClass(tx) {
      return tx?.kind || "";
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
      if (tx?.kind === "reward" && Array.isArray(tx.rewardFeeInputs)) {
        return tx.rewardFeeInputs.map((input) => ({
          ...input,
          rewardFee: true,
          outpoint: { txid: input.signature, index: "fee" },
        }));
      }
      return Array.isArray(tx.inputs) ? tx.inputs : [];
    },

    txVisualOutputs(tx) {
      if (tx?.kind === "reward" && Array.isArray(tx.rewardOutputs)) {
        return tx.rewardOutputs.map((output) => ({ ...output, kind: "reward" }));
      }
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
          kind: output.kind || "output",
          label: output.label || `Output ${index + 1}`,
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

    spendableTransferUtxos() {
      return this.hybridTransferUtxos().filter((utxo) => utxo.spendable !== false);
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
      if (this.lastSelectedTransferUtxo && !this.selectedTransferUtxos.includes(this.lastSelectedTransferUtxo)) {
        this.lastSelectedTransferUtxo = null;
      }
    },

    toggleTransferUtxoSelection(event, utxo) {
      const outpoint = this.utxoOutpoint(utxo);
      if (!utxo || utxo.spendable === false || !outpoint) {
        this.scheduleFeeEstimates();
        return;
      }

      this.rememberUtxoAmounts([utxo]);
      const spendable = this.spendableTransferUtxos();
      const outpoints = spendable.map((item) => this.utxoOutpoint(item));
      const currentIndex = outpoints.indexOf(outpoint);
      const anchorIndex = this.lastSelectedTransferUtxo
        ? outpoints.indexOf(this.lastSelectedTransferUtxo)
        : -1;

      const selected = new Set(this.selectedTransferUtxos);
      const checked = !selected.has(outpoint);
      if (event?.shiftKey && anchorIndex >= 0 && currentIndex >= 0) {
        const [from, to] = [anchorIndex, currentIndex].sort((left, right) => left - right);
        const range = spendable.slice(from, to + 1);
        this.rememberUtxoAmounts(range);
        for (const item of range) {
          const itemOutpoint = this.utxoOutpoint(item);
          if (checked) {
            selected.add(itemOutpoint);
          } else {
            selected.delete(itemOutpoint);
          }
        }
      } else if (checked) {
        selected.add(outpoint);
      } else {
        selected.delete(outpoint);
      }
      this.selectedTransferUtxos = Array.from(selected);

      this.lastSelectedTransferUtxo = outpoint;
      this.scheduleFeeEstimates();
    },

    async selectAllTransferUtxos() {
      try {
        const utxos = await this.fetchJson("/api/wallet/utxos/selectable");
        this.rememberUtxoAmounts(utxos);
        this.selectedTransferUtxos = utxos.map((utxo) => this.utxoOutpoint(utxo));
        this.lastSelectedTransferUtxo =
          this.selectedTransferUtxos[this.selectedTransferUtxos.length - 1] || null;
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
      this.lastSelectedTransferUtxo = null;
      this.scheduleFeeEstimates();
    },

    selectedTransferUtxoTotal() {
      return this.selectedTransferUtxos.reduce((sum, outpoint) => {
        return sum + this.microiunaAmount(this.selectedTransferUtxoAmounts[outpoint]);
      }, 0);
    },

    transferRequiredTotal() {
      return this.parseiunaAmount(this.transferAmount) + this.microiunaAmount(this.feeEstimates.transfer?.fee);
    },

    selectedTransferUtxosCoverTransfer() {
      return this.selectedTransferUtxoShortfall() === 0;
    },

    selectedTransferUtxoShortfall() {
      if (this.selectedTransferUtxos.length === 0) return 0;
      return Math.max(0, this.microiunaAmount(this.transferRequiredTotal()) - this.microiunaAmount(this.selectedTransferUtxoTotal()));
    },

    txInputAmountLabel(input) {
      return input.amount === null || input.amount === undefined ? "-" : `IUNA ${this.amountLabel(input.amount)}`;
    },

    txFeeRecipient(tx) {
      const context = this.selectedTransaction?.context || {};
      const address = tx.blockFinalizer ?? tx.blockMiner ?? context.blockFinalizer ?? context.blockMiner;
      return address ? this.addressLabel(address) : "future block finalizer";
    },

    selectedTransactionLabel() {
      if (!this.selectedTransaction) return "-";
      const { tx, context } = this.selectedTransaction;
      if (context.reward && context.blockHeight !== undefined) return `Block ${context.blockHeight} reward`;
      if (context.blockHeight !== undefined) return `Block ${context.blockHeight}`;
      if (tx?.status === "pending") return "Wallet pending";
      if (tx?.blockHeight !== null && tx?.blockHeight !== undefined) {
        return `Wallet block ${tx.blockHeight}`;
      }
      return context.source || "-";
    },

    blockLostIuna(block) {
      const explicitTotal = block?.lostIuna ?? block?.lost_iuna;
      if (explicitTotal !== null && explicitTotal !== undefined) return Number(explicitTotal) || 0;
      return this.blockTransactions(block)
        .filter((tx) => tx.kind === "burn")
        .reduce((sum, tx) => sum + this.txAmount(tx), 0);
    },

    blockTotalFees(block) {
      const explicitTotal = block?.totalFees ?? block?.total_fees ?? block?.reward;
      if (explicitTotal !== null && explicitTotal !== undefined) return Number(explicitTotal) || 0;
      return this.blockTransactions(block).reduce((sum, tx) => sum + Number(tx.fee || 0), 0);
    },

    blockRewardTransaction(block) {
      const inputs = this.blockTransactions(block)
        .filter((tx) => Number(tx.fee || 0) > 0)
        .map((tx) => ({
          rewardFee: true,
          transactionKind: tx.kind || "transaction",
          amount: tx.fee,
          owner: this.txFrom(tx) || this.txTo(tx) || null,
          signature: tx.signature,
          outpoint: { txid: tx.signature, index: "fee" },
        }));
      return {
        kind: "reward",
        signature: block.hash,
        amount: block.reward,
        fee: 0,
        inputs,
        outputs: this.blockRewardOutputs(block),
      };
    },

    blockRewardOutputs(block) {
      const reward = Math.max(0, Math.trunc(Number(block?.reward || 0)));
      if (reward === 0) return [];

      const bundles = Array.isArray(block?.burn_bundles)
        ? block.burn_bundles
        : (Array.isArray(block?.burnBundles) ? block.burnBundles : []);
      const committee = block?.finalizer_mode === "ticket" && Number(block?.finalizer_rank || 0) <= 1
        ? [...bundles]
            .sort((left, right) => Number(left.slot || 0) - Number(right.slot || 0))
            .filter((bundle) => bundle.member && bundle.member !== block.miner)
        : [];
      const committeePool = committee.length > 0 ? Math.floor(reward / 2) : 0;
      const outputs = [{
        kind: "reward",
        label: "Finalizer reward",
        amount: reward - committeePool,
        address: this.blockRewardAddress(block),
      }];

      let remaining = committeePool;
      committee.forEach((bundle, index) => {
        const membersLeft = committee.length - index;
        const amount = membersLeft === 1 ? remaining : Math.floor(remaining / membersLeft);
        remaining -= amount;
        if (amount > 0) {
          outputs.push({
            kind: "reward",
            label: `Committee reward (slot ${bundle.slot})`,
            amount,
            address: this.bundleRewardAddress(bundle),
          });
        }
      });
      return outputs;
    },

    blockRewardAddress(block) {
      return block?.rewardAddress ?? block?.reward_address ?? block?.miner ?? null;
    },

    bundleRewardAddress(bundle) {
      return bundle?.rewardAddress ?? bundle?.reward_address ?? bundle?.member ?? null;
    },

    blockTimestampLabel(block) {
      const timestamp = Number(block?.timestamp_ms ?? block?.timestampMs);
      if (Number(block?.height) === 0) return "Genesis";
      if (!Number.isFinite(timestamp) || timestamp <= 0) return "-";
      return new Date(timestamp).toLocaleString();
    },

    blockTotalBytes(block) {
      return Number(block?.totalBytes ?? block?.total_bytes ?? 0);
    },

    blockBurnBundleQuorum(block) {
      return block?.burnBundleQuorum ?? block?.burn_bundle_quorum ?? {};
    },

    blockBurnBundleRatio(block) {
      const quorum = this.blockBurnBundleQuorum(block);
      const included = Number(quorum.burnBundlesIncluded ?? quorum.burn_bundles_included ?? 0);
      const committeeSize = Number(quorum.committeeSize ?? quorum.committee_size ?? 0);
      return `${included}/${committeeSize}`;
    },

    blockBurnBundleSlots(block) {
      if (!block || block.finalizer_mode === "recovery") return [];
      const bundles = block.burn_bundles || block.burnBundles || [];
      return [
        {
          slot: 0,
          member: block.miner,
          rewardAddress: block.rewardAddress ?? block.reward_address ?? null,
          role: "Finalizer",
          implicit: true,
          burns: this.blockTransactions(block).filter((tx) => tx.kind === "burn"),
          byteSize: 0,
          hash: "",
        },
        ...bundles.map((bundle) => ({
          slot: Number(bundle.slot),
          member: bundle.member,
          rewardAddress: bundle.rewardAddress ?? bundle.reward_address ?? null,
          role: "Committee member",
          implicit: false,
          burns: bundle.burns || [],
          byteSize: Number(bundle.byte_size ?? bundle.byteSize ?? 0),
          hash: bundle.hash || "",
        })),
      ].sort((left, right) => left.slot - right.slot);
    },

    burnBundleSlotDetail(slot) {
      if (slot?.implicit) return "Implicit block attestation";
      const count = slot?.burns?.length || 0;
      return `${count} burn${count === 1 ? "" : "s"} · ${slot?.byteSize || 0}B gossip JSON`;
    },

    blockByteBreakdown(block) {
      const transactionRows = this.blockTransactionByteBreakdown(block);
      return [
        ["Header and proof", Number(block?.headerAndProofBytes ?? block?.header_and_proof_bytes ?? 0), ""],
        ...(transactionRows.length
          ? transactionRows
          : [["Transactions", Number(block?.transactionBytes ?? block?.transaction_bytes ?? 0), ""]]),
        ["Burn bundles", Number(block?.burnBundleBytes ?? block?.burn_bundle_bytes ?? 0), "burn"],
      ];
    },

    blockTransactionByteBreakdown(block) {
      const rows = block?.transactionByteBreakdown ?? block?.transaction_byte_breakdown;
      if (!Array.isArray(rows)) return [];
      return rows
        .map((row) => {
          const label = row.label || row.kind || "transaction";
          return [label, Number(row.bytes ?? 0), label];
        })
        .filter((row) => row[1] > 0);
    },

    recentBlockFeeAverage(count) {
      const sample = this.blocks.filter((block) => block.height > 0).slice(0, count);
      if (sample.length === 0) return 0;
      return Math.round(sample.reduce((sum, block) => sum + this.blockTotalFees(block), 0) / sample.length);
    },

    blockBurnCount(block) {
      const count = Number(block?.burnCount ?? block?.burn_count);
      if (Number.isFinite(count)) return count;
      return this.blockTransactions(block).filter((tx) => tx.kind === "burn").length;
    },

    blockTransferCount(block) {
      const count = Number(block?.transferCount ?? block?.transfer_count);
      if (Number.isFinite(count)) return count;
      return this.blockTransactions(block).filter((tx) => tx.kind === "transfer").length;
    },

    blockMineCount(block) {
      const count = Number(block?.mineCount ?? block?.mine_count);
      if (Number.isFinite(count)) return count;
      return this.blockTransactions(block).filter((tx) => tx.kind === "mine").length;
    },

    blockTransactions(block) {
      return block?.transactions || [];
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
      const finalizer = this.shortAddressLabel(block.miner);
      const owner = finalizer;
      return block.finalizer_mode === "recovery" ? `${owner} · Recovery` : owner;
    },

    buildingBlockHeight() {
      const height = Number(this.status.chain?.height);
      return Number.isSafeInteger(height) && height >= 0 ? height + 1 : "-";
    },

    buildingBlockOverview() {
      return {
        burnCount: Number(this.status.chain?.pending_burns ?? 0),
        transferCount: Number(this.status.chain?.pending_transfers ?? 0),
        mineCount: Number(this.status.chain?.pending_mines ?? 0),
        miner: this.currentFinalizerAddress(),
      };
    },

    burnLeaderRanks(block) {
      if (Array.isArray(block?.burn_leader_ranks)) return block.burn_leader_ranks;
      return Array.isArray(block?.burnLeaderRanks) ? block.burnLeaderRanks : [];
    },

    burnLeaderRanksLoading(block) {
      return !this.burnLeaderRanksError && this.burnLeaderRanksPending(block);
    },

    burnLeaderRanksPending(block) {
      return block?.burn_leader_ranks_loaded === false;
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

    walletTxIsIncoming(tx) {
      return ["received", "reward", "migrated"].includes(tx?.direction);
    },

    txTitle(tx) {
      if (tx.status === "pending") return "Pending";
      return tx.blockHeight === null ? "Confirmed" : `Block ${tx.blockHeight}`;
    },

    walletTxTimeLabel(tx) {
      const timestamp = Number(tx?.timestampMs ?? tx?.timestamp_ms);
      if (!Number.isFinite(timestamp) || timestamp <= 0) {
        return tx?.status === "pending" ? "Pending" : "-";
      }
      return new Date(timestamp).toLocaleString();
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

    syncingNode() {
      return (
        this.canUseProtectedApi() &&
        !this.showingNetworkMigration() &&
        !this.chainResetModalOpen &&
        this.config.setup_complete === true &&
        this.networkHealthLoaded &&
        this.networkHealth.state === "syncing"
      );
    },

    syncCurrentHeight() {
      const height = Number(
        this.networkHealth.sync_validated_height ??
          this.networkHealth.local_height ??
          this.status.chain?.height ??
          0
      );
      return Number.isFinite(height) && height >= 0 ? Math.floor(height) : 0;
    },

    syncTargetHeight() {
      const batchTarget = Number(this.networkHealth.sync_target_height ?? 0);
      const knownTarget = Number(this.networkHealth.best_known_height ?? 0);
      const target = Math.max(
        Number.isFinite(batchTarget) ? batchTarget : 0,
        Number.isFinite(knownTarget) ? knownTarget : 0,
        this.syncCurrentHeight()
      );
      return Number.isFinite(target) && target >= 0
        ? Math.max(this.syncCurrentHeight(), Math.floor(target))
        : this.syncCurrentHeight();
    },

    syncProgressPercent() {
      const current = this.syncCurrentHeight();
      const target = this.syncTargetHeight();
      if (target <= 0) return 0;
      return Math.max(0, Math.min(100, (current / target) * 100));
    },

    syncProgressLabel() {
      return `Syncing ${this.syncCurrentHeight().toLocaleString()} of ${this.syncTargetHeight().toLocaleString()}`;
    },

    networkLagLabel() {
      const lag = this.networkHealth.lag_blocks;
      if (typeof lag !== "number") return "-";
      if (lag === 0) return "even";
      return `${lag} behind`;
    },

    networkTipLabel() {
      return this.short(this.networkHealth.local_tip_hash);
    },

    networkLastBlockAgeLabel() {
      const ageMs = this.networkHealth.last_block_age_ms;
      if (typeof ageMs !== "number" || !Number.isFinite(ageMs)) return "-";
      return this.durationLabel(ageMs);
    },

    dashboardMiningState() {
      return this.powMiningEnabled ? "good" : "neutral";
    },

    dashboardMiningLabel() {
      if (!this.powMiningEnabled) return "Off";
      return `${this.powMiningWorkers} worker${this.powMiningWorkers === 1 ? "" : "s"}`;
    },

    dashboardBurnState() {
      return this.miningEnabled ? "good" : "neutral";
    },

    dashboardBurnLabel() {
      if (!this.miningEnabled) return "Off";
      return `IUNA ${this.amountLabel(this.burnAmount)} / blk`;
    },

    dashboardBlockState() {
      const ageMs = this.networkHealth.last_block_age_ms;
      if (typeof ageMs !== "number" || !Number.isFinite(ageMs)) return "neutral";
      if (ageMs <= 20 * 60 * 1000) return "good";
      if (ageMs <= 60 * 60 * 1000) return "warning";
      return "bad";
    },

    dashboardLastBlockLabel() {
      const age = this.networkLastBlockAgeLabel();
      return age === "-" ? "Unavailable" : `${age} ago`;
    },

    dashboardNetworkState() {
      const state = this.networkHealth.state;
      if (!state) return "neutral";
      if (state === "healthy" || state === "ahead of peers") return "good";
      if (
        state === "syncing" ||
        state === "mempool syncing" ||
        state === "stale" ||
        state === "peer errors" ||
        state === "banned"
      ) {
        return "warning";
      }
      return "bad";
    },

    networkFinalizerLabel() {
      return this.shortAddressLabel(this.networkHealth.current_leader);
    },

    networkFinalizerModeLabel() {
      const mode = this.networkHealth.last_finalizer_mode;
      if (!mode) return "-";
      const rank = this.networkHealth.last_finalizer_rank;
      const label = mode.charAt(0).toUpperCase() + mode.slice(1);
      return typeof rank === "number" ? `${label} #${rank}` : label;
    },

    networkVdfLabel() {
      const rounds = this.networkHealth.vdf_rounds;
      const targetMs = this.networkHealth.vdf_target_block_ms;
      if (typeof rounds !== "number") return "-";
      const target = typeof targetMs === "number" ? ` / ${this.durationLabel(targetMs)}` : "";
      return `${rounds.toLocaleString()}${target}`;
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
      return this.peers.filter((peer) =>
        !peer.last_error
        && !this.bannedPeer(peer)
        && !this.stalePeer(peer)
        && typeof peer.last_success_ms === "number"
        && this.peerOnLocalTip(peer)
      );
    },

    peerOnLocalTip(peer) {
      return typeof peer.last_known_height === "number"
        && peer.last_known_height === this.networkHealth.local_height
        && typeof peer.last_known_tip_hash === "string"
        && peer.last_known_tip_hash === this.networkHealth.local_tip_hash;
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
      if (typeof peer.last_known_height === "number") {
        const localHeight = this.networkHealth.local_height;
        if (typeof localHeight === "number" && peer.last_known_height < localHeight) return "behind";
        if (typeof localHeight === "number" && peer.last_known_height > localHeight) return "ahead";
        if (typeof localHeight === "number"
            && typeof peer.last_known_tip_hash === "string"
            && typeof this.networkHealth.local_tip_hash === "string"
            && peer.last_known_tip_hash !== this.networkHealth.local_tip_hash) return "forked";
        return "synced";
      }
      if ((peer.messages_sent ?? 0) > 0 || (peer.messages_received ?? 0) > 0) return "active";
      return "pending";
    },

    peerStatusLabel(peer) {
      return {
        error: "Error",
        banned: "Banned",
        forked: "Forked",
        ahead: "Ahead",
        behind: "Behind",
        stale: "Stale",
        synced: "Synced",
        active: "Active",
        pending: "Pending",
      }[this.peerStatus(peer)];
    },

    relativeTimeLabel(timestampMs) {
      if (typeof timestampMs !== "number" || !Number.isFinite(timestampMs)) return "-";
      const ageSeconds = Math.max(0, Math.round((Date.now() - timestampMs) / 1000));
      if (ageSeconds < 5) return "now";
      if (ageSeconds < 60) return `${ageSeconds}s ago`;
      const ageMinutes = Math.round(ageSeconds / 60);
      if (ageMinutes < 60) return `${ageMinutes}m ago`;
      const ageHours = Math.round(ageMinutes / 60);
      if (ageHours < 48) return `${ageHours}h ago`;
      return `${Math.round(ageHours / 24)}d ago`;
    },

    durationLabel(durationMs) {
      if (typeof durationMs !== "number" || !Number.isFinite(durationMs)) return "-";
      const seconds = Math.max(0, Math.round(durationMs / 1000));
      if (seconds < 60) return `${seconds}s`;
      const minutes = Math.round(seconds / 60);
      if (minutes < 60) return `${minutes}m`;
      const hours = Math.round(minutes / 60);
      if (hours < 48) return `${hours}h`;
      return `${Math.round(hours / 24)}d`;
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

    stratumRuntimeEnabled() {
      return this.status.stratum?.enabled === true;
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
