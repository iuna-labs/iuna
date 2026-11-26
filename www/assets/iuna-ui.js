const IUNA_DOWNLOADS_URL = "https://getiuna.org/downloads/";
const IUNA_RELEASE_METADATA_URL = "https://getiuna.org/downloads/latest.json";
const IUNA_RELEASE_CHECK_INTERVAL_MS = 30 * 60 * 1000;
const IUNA_CHART_MAX_POINTS = 300;
// A node GET that has not answered by then is treated as failed.
const IUNA_REQUEST_TIMEOUT_MS = 20_000;
const iunaChartCaches = new WeakMap();
const iunaMetricsRequests = new WeakMap();
const iunaRefreshRequests = new WeakMap();
// The node names the parts a reward collects; a note explains a block-wide amount.
const IUNA_REWARD_INPUTS = {
  ledger_fee_share: { label: "Ledger fee share", note: "Share of this block's Ledger transaction fees." },
  inclusion_fee: { label: "Inclusion fee", note: "Fees of the tickets included in this Heartbeat." },
  expenditure: { label: "Ticket expenditure", note: "Budget slices paid out by active mining tickets in this Heartbeat." },
  subsidy: { label: "Subsidy", note: "Newly minted by this Ledger block." },
};
// A Ledger reward pays its subsidy and its fee share as two outputs of one settlement.
const IUNA_REWARD_OUTPUTS = { heartbeat_payout: "Heartbeat reward", subsidy: "Subsidy", ledger_fee_share: "Ledger fee share" };
// Accent colors offered in Settings; node.html defines each one's tokens under [data-accent].
const IUNA_ACCENTS = [
  { id: "lime", label: "Lime" },
  { id: "orange", label: "Orange" },
  { id: "magenta", label: "Magenta" },
  { id: "cobalt", label: "Cobalt" },
  { id: "bone", label: "Bone" },
];
const IUNA_DEFAULT_ACCENT = "lime";

function iunaStoredAccent() {
  try {
    const stored = localStorage.getItem("iunaAccent");
    return IUNA_ACCENTS.some((option) => option.id === stored) ? stored : IUNA_DEFAULT_ACCENT;
  } catch {
    return IUNA_DEFAULT_ACCENT;
  }
}

// Runs before Alpine shows the page (the body is x-cloak until then), so the default never flashes.
function iunaApplyAccent(accent) {
  const root = typeof document === "undefined" ? null : document.documentElement;
  if (root) root.dataset.accent = accent;
}
iunaApplyAccent(iunaStoredAccent());

window.iunaApp = function iunaApp() {
  return {
    tab: "dashboard",
    accent: iunaStoredAccent(),
    // The Cmd/Ctrl+K palette. `requestId` drops lookups that finish after the query changed.
    palette: { open: false, query: "", results: [], active: 0, busy: false, requestId: 0, trigger: null },
    // Bumped by each palette choice, so a slow lookup from an earlier choice cannot navigate.
    paletteNavigation: 0,
    status: {},
    peers: [],
    // False until the first peer list arrives, so an empty list is not shown as isolation.
    peersLoaded: false,
    // The latest peer request failed; until a list arrives this shows instead of "Loading".
    peersError: false,
    peerTargets: [],
    peerForm: { address: "", busy: false },
    activity: null,
    activityVisibleCount: 25,
    activityBusy: false,
    activityPageDepth: 0,
    activityObserver: null,
    activityFilters: { transfers: true, tickets: true, heartbeatRewards: true, ledgerRewards: true },
    // Which list the wallet's right-hand panel shows: activity, contacts, utxos or addresses.
    walletView: "activity",
    utxosBusy: false,
    utxosLoading: false,
    // Pages of /api/wallet/utxos, largest output first: { public_key, tip_hash, total, utxos, next_offset }.
    utxoPage: null,
    utxoPageKey: null,
    walletAddresses: null,
    walletAddressesBusy: false,
    walletAddressesError: null,
    walletAddressFilter: "used",
    contacts: [],
    contactForm: { name: "", address: "" },
    contactModalOpen: false,
    contactEditingAddress: null,
    passwordForm: { current: "", next: "", confirmation: "", busy: false },
    p2pForm: { publicEnabled: false, bindPort: 9444, publicAddress: "", busy: false, dirty: false },
    heartbeats: [],
    ledgerBlocks: [],
    mempool: { ledger_transactions: [], mining_tickets: [] },
    metricsRows: [],
    metricsBusy: false,
    metricsRequestId: 0,
    metricsRange: (() => {
      try {
        const stored = localStorage.getItem("iunaMetricsRange");
        if (stored === "1000") return 1000;
        if (stored === "all") return "all";
      } catch {}
      return 100;
    })(),
    chainView: "ledger",
    heartbeatPage: { limit: 100, busy: false, hasMore: true },
    selectedTransaction: null,
    transactionModalTrigger: null,
    selectedBlock: null,
    selectedBlockDetail: null,
    selectedBlockChain: null,
    selectedBlockFollowsLatest: false,
    // Opening the Chain screen selects the newest Ledger block once its blocks are loaded.
    chainDefaultSelectionPending: false,
    blockDetailBusy: false,
    blockDetailRequestId: 0,
    showSetup: false,
    showLogin: false,
    setupStep: 1,
    setupReady: false,
    // Set once init finished its first refresh; later refreshes may then complete setup's start.
    initialized: false,
    // The operator chose to use the screen while the node still does not answer.
    startupDismissed: false,
    lastUpdated: null,
    pollTimer: null,
    releaseCheckTimer: null,
    pollFailures: 0,
    operatorToken: "",
    tokenStored: false,
    flash: null,
    flashTimer: null,
    latestRelease: null,
    releaseCheckState: "idle",
    releaseCheckError: null,
    desktopUpdateModalOpen: false,
    desktopUpdateBusy: false,
    transfer: { recipient: "", amount: "", fee: "0.00001", busy: false },
    pendingTransfer: null,
    shortcutKeyHeld: false,
    chartHover: null,
    mining: { busy: false, workers: 1, message: "Heartbeat mining is off", hashes: null, sampledAt: null, hashrate: null },
    ticket: { budget: "", duration: 10, fee: "0.00000001", quote: null, continuousQuote: null, busy: false, continuousBusy: false },
    walletAccess: {
      mode: "",
      phase: "credentials",
      password: "",
      confirmation: "",
      recoveryInput: "",
      recoveryPhrase: "",
      confirmationWords: ["", "", ""],
      confirmationIndices: [2, 11, 20],
      busy: false,
    },

    async init() {
      this.captureDesktopToken();
      this.tokenStored = Boolean(this.storedOperatorToken());
      this.contacts = this.loadContacts();
      this.tab = this.tabFromHash();
      this.chainDefaultSelectionPending = this.tab === "chain";
      window.addEventListener("hashchange", () => this.setTab(this.tabFromHash()));
      window.addEventListener("keydown", (event) => this.handleKeydown(event));
      window.addEventListener("keyup", (event) => this.handleKeyup(event));
      window.addEventListener("blur", () => { this.shortcutKeyHeld = false; });
      document.addEventListener("visibilitychange", () => {
        if (!document.hidden) this.refresh(true).then(() => this.schedulePoll());
      });
      await this.refresh();
      // Polling and setup start before the ticket quote: a quote that hangs on a busy node must
      // not keep the startup screen without its Retry and Continue buttons.
      this.initialized = true;
      this.prepareSetup();
      this.schedulePoll();
      this.$watch("setupReady", (ready) => {
        if (ready) this.$nextTick(() => (this.showSetup ? this.$refs.setupModal : this.$refs.loginPassword)?.focus());
      });
      this.$watch("showSetup", (open) => {
        if (open) this.$nextTick(() => this.$refs.setupModal?.focus());
      });
      this.$watch("showLogin", (open) => {
        if (open) this.$nextTick(() => this.$refs.loginPassword?.focus());
      });
      if (this.showSetup) this.$nextTick(() => this.$refs.setupModal?.focus());
      if (this.showLogin) this.$nextTick(() => this.$refs.loginPassword?.focus());
      this.checkLatestRelease();
      this.scheduleReleaseCheck();
      await this.quoteTicket(true);
    },

    captureDesktopToken() {
      const match = window.location.hash.match(/(?:^#|&)operator-token=([^&]+)/);
      if (!match) return;
      try { sessionStorage.setItem("iunaOperatorToken", decodeURIComponent(match[1])); } catch {}
      history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    },

    tabs() {
      return ["dashboard", "wallet", "mining", "p2p", "chain", "metrics", "settings"];
    },

    tabFromHash() {
      const value = window.location.hash.replace(/^#\/?/, "");
      return this.tabs().includes(value) ? value : "dashboard";
    },

    setTab(tab) {
      if (!this.tabs().includes(tab)) return;
      const changed = this.tab !== tab;
      if (changed && this.tab === "metrics") this.cancelMetricsRefresh();
      this.tab = tab;
      if (window.location.hash !== `#${tab}`) window.location.hash = tab;
      if (changed && (tab === "wallet" || tab === "dashboard")) this.refresh(true);
      if (changed && tab === "metrics") this.refreshMetrics();
      if (changed && tab === "chain") {
        this.chainDefaultSelectionPending = true;
        this.selectDefaultChainBlock();
        this.$nextTick(() => this.ensureChainRailScrollable(this.chainView));
      }
    },

    // Cmd (macOS) or Ctrl plus 1-7 opens the matching sidebar screen; badges show while held.
    shortcutModifier(event) {
      return /Mac|iPhone|iPad/.test(navigator.platform || "") ? event.metaKey : event.ctrlKey;
    },

    shortcutsAvailable() {
      return !this.startupWaiting()
        && !(this.setupReady && (this.showSetup || this.showLogin))
        && !this.palette.open
        && !this.contactModalOpen
        && !this.desktopUpdateModalOpen
        && !this.pendingTransfer
        && !this.selectedTransaction;
    },

    shortcutNumber(tab) {
      return this.tabs().indexOf(tab) + 1;
    },

    handleKeydown(event) {
      if (event.key === "Escape") {
        this.closeModals();
        return;
      }
      if (this.shortcutModifier(event) && !event.altKey && !event.shiftKey && String(event.key).toLowerCase() === "k") {
        if (this.palette.open) {
          event.preventDefault();
          this.closePalette();
        } else if (this.shortcutsAvailable()) {
          event.preventDefault();
          this.openPalette();
        }
        return;
      }
      if (["Meta", "Control"].includes(event.key)) {
        this.shortcutKeyHeld = this.shortcutModifier(event) && this.shortcutsAvailable();
        return;
      }
      if (!this.shortcutModifier(event) || event.altKey || event.shiftKey || !this.shortcutsAvailable()) return;
      const match = /^Digit([1-9])$/.exec(event.code || "") || /^([1-9])$/.exec(event.key || "");
      const tab = match && this.tabs()[Number(match[1]) - 1];
      if (!tab) return;
      event.preventDefault();
      this.setTab(tab);
    },

    handleKeyup(event) {
      if (!this.shortcutModifier(event)) this.shortcutKeyHeld = false;
    },

    closeModals() {
      if (this.palette.open) {
        this.closePalette();
        return;
      }
      if (this.showLogin) return;
      if (this.selectedTransaction) {
        this.closeTransactionModal();
        return;
      }
      if (this.contactModalOpen) {
        this.closeContactEditor();
        return;
      }
      if (this.desktopUpdateModalOpen && !this.desktopUpdateBusy) {
        this.desktopUpdateModalOpen = false;
        return;
      }
      if (this.pendingTransfer && !this.transfer.busy) {
        this.pendingTransfer = null;
        return;
      }
      if (this.showSetup) {
        this.closeSetup();
        return;
      }
      if (this.selectedBlock) {
        this.clearSelectedBlock();
      }
    },

    pageTitle(tab = this.tab) {
      return {
        dashboard: "Dashboard",
        wallet: "Wallet",
        mining: "Mining",
        p2p: "P2P",
        chain: "Chain",
        metrics: "Metrics",
        settings: "Settings",
      }[tab] || "iuna";
    },

    // Setup and login wait for a status that names this node. A slow or still-starting node can
    // fail its first status request, and deciding then opened Initial Setup over an existing wallet.
    // The waiting screen covers everything until the node answers, unless the operator skips it.
    startupWaiting() {
      return !this.setupReady && !this.startupDismissed;
    },

    async retryStartup() {
      await this.refresh(true);
      this.schedulePoll();
    },

    prepareSetup() {
      if (this.setupReady || !/^[0-9a-f]{32}$/.test(String(this.status.node_instance_id || ""))) return;
      this.setupReady = true;
      this.showSetup = !this.setupCompleted();
      this.syncLoginVisibility();
    },

    setupCompleted() {
      const instance = String(this.status.node_instance_id || "");
      if (!/^[0-9a-f]{32}$/.test(instance)) return false;
      try { return localStorage.getItem(`iunaSetupComplete:${instance}`) === "true"; } catch { return false; }
    },

    completeSetup() {
      const instance = String(this.status.node_instance_id || "");
      if (/^[0-9a-f]{32}$/.test(instance)) {
        try { localStorage.setItem(`iunaSetupComplete:${instance}`, "true"); } catch {}
      }
      this.showSetup = false;
      this.setupStep = 1;
      this.setTab("dashboard");
      this.notify("Setup complete");
      this.syncLoginVisibility();
    },

    // Contacts live only in this browser; they are a convenience, not wallet data.
    loadContacts() {
      try {
        const stored = JSON.parse(localStorage.getItem("iunaContacts") || "[]");
        return Array.isArray(stored) ? stored.filter((contact) => /^[0-9a-f]{64}$/.test(contact?.address)) : [];
      } catch { return []; }
    },

    saveContacts() {
      try { localStorage.setItem("iunaContacts", JSON.stringify(this.contacts)); } catch {}
    },

    addContact(name = this.contactForm.name, address = this.contactForm.address) {
      const key = String(address).trim().toLowerCase();
      const label = String(name).trim().slice(0, 40);
      if (!label) return this.notify("Give the contact a name", "error");
      if (!/^[0-9a-f]{64}$/.test(key)) return this.notify("A contact needs a 64-character address", "error");
      if (this.contacts.some((contact) => contact.address === key)) return this.notify("That address is already a contact", "error");
      this.contacts = [...this.contacts, { name: label, address: key }].sort((a, b) => a.name.localeCompare(b.name));
      this.saveContacts();
      this.contactForm = { name: "", address: "" };
      this.notify(`Saved ${label}`);
    },

    openContactEditor(address) {
      const key = String(address || "").trim().toLowerCase();
      if (!/^[0-9a-f]{64}$/.test(key)) return;
      const existing = this.contacts.find((contact) => contact.address === key);
      this.contactEditingAddress = existing?.address || null;
      this.contactForm = { name: existing?.name || "", address: key };
      this.contactModalOpen = true;
      this.$nextTick(() => this.$refs.contactName?.focus());
    },

    closeContactEditor() {
      this.contactModalOpen = false;
      this.contactEditingAddress = null;
      this.contactForm = { name: "", address: "" };
    },

    saveContactEditor() {
      const key = String(this.contactForm.address).trim().toLowerCase();
      const label = String(this.contactForm.name).trim().slice(0, 40);
      if (!label) return this.notify("Give the contact a name", "error");
      if (!/^[0-9a-f]{64}$/.test(key)) return this.notify("A contact needs a 64-character address", "error");
      const duplicate = this.contacts.some((contact) => (
        contact.address === key && contact.address !== this.contactEditingAddress
      ));
      if (duplicate) return this.notify("That address is already a contact", "error");
      this.contacts = [
        ...this.contacts.filter((contact) => contact.address !== this.contactEditingAddress),
        { name: label, address: key },
      ].sort((left, right) => left.name.localeCompare(right.name));
      this.saveContacts();
      this.closeContactEditor();
      this.notify(`Saved ${label}`);
    },

    removeContact(address) {
      this.contacts = this.contacts.filter((contact) => contact.address !== address);
      this.saveContacts();
      if (this.contactEditingAddress === address) this.closeContactEditor();
    },

    contactName(address) {
      return this.contacts.find((contact) => contact.address === address)?.name || "";
    },

    resetSetup() {
      this.setupStep = 1;
      this.showSetup = true;
    },

    closeSetup() {
      if (!this.setupCompleted()) return;
      this.showSetup = false;
      this.setupStep = 1;
      this.syncLoginVisibility();
    },

    syncLoginVisibility() {
      // A backup needs an unlocked wallet; left set, the unlock form would submit a backup.
      if (this.walletAccess.mode === "backup" && !this.status.wallet_unlocked) {
        this.walletAccess.recoveryPhrase = "";
        this.selectWalletMode("");
      }
      const shouldOpen = this.setupReady
        && !this.showSetup
        && this.status.wallet_exists === true
        && this.status.wallet_unlocked !== true;
      if (shouldOpen && !this.showLogin) this.selectWalletMode("");
      if (shouldOpen && this.palette.open) this.closePalette(false);
      this.showLogin = shouldOpen;
    },

    nextSetupStep() {
      this.setupStep = Math.min(3, this.setupStep + 1);
    },

    previousSetupStep() {
      this.setupStep = Math.max(1, this.setupStep - 1);
    },

    // The node reports a locked wallet's existence, but none of its keys, funds or history.
    walletNeedsCreation() {
      return this.status.wallet_exists !== true;
    },

    walletActionLabel() {
      if (this.walletAccess.busy) return this.walletAccess.mode === "restore" ? "Restoring wallet…" : (this.walletNeedsCreation() ? "Creating wallet…" : "Unlocking wallet…");
      if (!this.walletNeedsCreation()) return "Unlock wallet";
      return this.walletAccess.mode === "restore" ? "Restore wallet" : "Create wallet";
    },

    selectWalletMode(mode) {
      this.walletAccess.mode = mode;
      this.walletAccess.phase = "credentials";
      this.walletAccess.password = "";
      this.walletAccess.confirmation = "";
      this.walletAccess.recoveryInput = "";
    },

    startWalletBackup() {
      this.selectWalletMode("backup");
    },

    recoveryWords() {
      return this.walletAccess.recoveryPhrase.trim().split(/\s+/).filter(Boolean);
    },

    startRecoveryCheck() {
      this.walletAccess.confirmationIndices = this.randomWordIndices(3, this.recoveryWords().length || 24);
      this.walletAccess.confirmationWords = ["", "", ""];
      this.walletAccess.phase = "confirm";
    },

    verifyRecoveryPhrase() {
      const words = this.recoveryWords();
      const valid = this.walletAccess.confirmationIndices.every((index, position) =>
        (this.walletAccess.confirmationWords[position] || "").trim().toLowerCase() === words[index - 1]
      );
      if (!valid) {
        this.notify("Those words do not match your recovery phrase", "error");
        return;
      }
      this.walletAccess.recoveryPhrase = "";
      this.walletAccess.confirmationWords = ["", "", ""];
      this.walletAccess.phase = "complete";
      this.walletAccess.mode = "";
      this.notify("Recovery phrase confirmed");
    },

    async accessWallet() {
      if (this.walletAccess.busy) return;
      if (this.walletNeedsCreation() && !["create", "restore"].includes(this.walletAccess.mode)) {
        this.notify("Choose whether to create or restore a wallet", "error");
        return;
      }
      // Matches MIN_NEW_PASSWORD_CHARS in the wallet store; unlocking accepts existing passwords.
      const choosingPassword = this.walletNeedsCreation() && this.walletAccess.mode !== "backup";
      if (choosingPassword && [...this.walletAccess.password].length < 10) {
        this.notify("Use a wallet password of at least 10 characters", "error");
        return;
      }
      if (!this.walletAccess.password) {
        this.notify("Enter your wallet password", "error");
        return;
      }
      if (this.walletNeedsCreation() && this.walletAccess.password !== this.walletAccess.confirmation) {
        this.notify("The wallet passwords do not match", "error");
        return;
      }
      const recoveryPhrase = this.walletAccess.recoveryInput.trim().toLowerCase().replace(/\s+/g, " ");
      if (this.walletAccess.mode === "restore" && recoveryPhrase.split(" ").length !== 24) {
        this.notify("Enter the complete 24-word recovery phrase", "error");
        return;
      }
      this.walletAccess.busy = true;
      try {
        const backup = this.walletAccess.mode === "backup" && this.status.wallet_unlocked;
        const action = backup ? "backup" : (this.walletNeedsCreation() ? this.walletAccess.mode : "unlock");
        const result = await this.postJson("/api/wallet", {
          password: this.walletAccess.password,
          action,
          recovery_phrase: action === "restore" ? recoveryPhrase : undefined,
        });
        this.walletAccess.password = "";
        this.walletAccess.confirmation = "";
        this.walletAccess.recoveryInput = "";
        if (result.recovery_phrase) {
          this.walletAccess.recoveryPhrase = result.recovery_phrase;
          this.walletAccess.phase = "backup";
        } else {
          this.walletAccess.phase = "complete";
        }
        // The answer already says the wallet is open. Waiting for the next full refresh to close the
        // login would keep it up whenever any unrelated part of that refresh fails.
        if (action !== "backup") this.status = { ...this.status, wallet_unlocked: true };
        this.syncLoginVisibility();
        await this.refresh(true);
        this.notify(action === "backup" ? "Recovery phrase ready — save it safely" : (action === "restore" ? "Wallet restored and encrypted" : (result.created ? "Wallet created — save your recovery phrase" : "Wallet unlocked")));
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.walletAccess.busy = false;
      }
    },

    randomWordIndices(count, total) {
      const indices = new Set();
      while (indices.size < count) indices.add(1 + (crypto.getRandomValues(new Uint32Array(1))[0] % total));
      return [...indices].sort((left, right) => left - right);
    },

    canContinueSetup() {
      if (this.setupStep !== 2) return true;
      return this.status.wallet_unlocked === true && !this.walletAccess.recoveryPhrase;
    },

    // Poll every 5 seconds, back off up to a minute while the node is unreachable, and stop
    // while the tab is hidden (visibilitychange resumes it).
    schedulePoll() {
      clearTimeout(this.pollTimer);
      if (document.hidden) return;
      // While the waiting screen is up, back-off would keep it there long after the node answers.
      const delay = this.startupWaiting() ? 3000 : Math.min(60_000, 5000 * 2 ** this.pollFailures);
      this.pollTimer = setTimeout(async () => {
        await this.refresh(true);
        this.schedulePoll();
      }, delay);
    },

    scheduleReleaseCheck() {
      clearTimeout(this.releaseCheckTimer);
      this.releaseCheckTimer = setTimeout(async () => {
        await this.checkLatestRelease();
        this.scheduleReleaseCheck();
      }, IUNA_RELEASE_CHECK_INTERVAL_MS);
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
      return Boolean(current && latest && current !== latest && this.compareVersions(latest, current) > 0);
    },

    versionPanelTitle() {
      if (this.desktopUpdateBusy) return "Installing update";
      if (this.updateAvailable()) return `Update available: ${this.latestReleaseLabel()}`;
      if (this.releaseCheckState === "failed") return this.releaseCheckError || "Could not check latest release";
      if (this.releaseCheckState === "checking") return "Checking latest release";
      return "iuna is up to date";
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
        if (!response.ok) throw new Error(`Release check failed (${response.status})`);
        const release = await response.json();
        const version = this.normalizeVersion(release.tag || release.version);
        if (!version) throw new Error("Release metadata is missing a version");
        let desktopVersion = null;
        const invoke = window.__TAURI__?.core?.invoke;
        if (typeof invoke === "function") {
          try { desktopVersion = this.normalizeVersion(await invoke("check_desktop_update")); } catch {}
        }
        this.latestRelease = {
          tag: `v${version}`,
          url: release.url || IUNA_DOWNLOADS_URL,
          desktopReady: desktopVersion === version,
        };
        this.releaseCheckState = "done";
      } catch (error) {
        this.releaseCheckError = error?.message || "Release check failed";
        this.releaseCheckState = "failed";
      }
    },

    async openLatestRelease() {
      const url = this.latestRelease?.url || IUNA_DOWNLOADS_URL;
      const invoke = window.__TAURI__?.core?.invoke;
      if (this.updateAvailable() && this.latestRelease?.desktopReady === true && typeof invoke === "function") {
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
      if (!this.desktopUpdateBusy) this.desktopUpdateModalOpen = false;
    },

    async installDesktopUpdate() {
      const invoke = window.__TAURI__?.core?.invoke;
      if (typeof invoke !== "function" || this.desktopUpdateBusy) return;
      this.desktopUpdateBusy = true;
      try {
        await invoke("install_desktop_update");
      } catch (error) {
        this.desktopUpdateBusy = false;
        this.desktopUpdateModalOpen = false;
        this.notify(error?.message || String(error) || "Desktop update failed", "error");
      }
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
        const difference = (leftParts[index] || 0) - (rightParts[index] || 0);
        if (difference !== 0) return difference;
      }
      return 0;
    },

    // Refreshes never overlap: a Retry or a returning tab used to start a second one, and whichever
    // finished last overwrote the other's newer state. A refresh asked for while one runs is run
    // once more afterwards, so a caller that just changed something still sees its result.
    refresh(silent = false) {
      let state = iunaRefreshRequests.get(this);
      if (!state) iunaRefreshRequests.set(this, (state = { running: null, again: null, silent: true }));
      if (!state.running) {
        state.running = this.runRefresh(silent).finally(() => { state.running = null; });
        return state.running;
      }
      state.silent = state.silent && silent;
      state.again ||= state.running.then(() => {
        const quiet = state.silent;
        state.again = null;
        state.silent = true;
        return this.refresh(quiet);
      });
      return state.again;
    },

    async runRefresh(silent) {
      let failures = [];
      try {
        const previousHeartbeatHead = this.heartbeats[0]?.block_hash;
        const previousLedgerHead = this.ledgerBlocks[0]?.block_hash;
        const heartbeatRail = this.chainRailState("heartbeatRail");
        const ledgerRail = this.chainRailState("ledgerRail");
        // Each response is applied on its own: one bad response used to abort the whole refresh,
        // which left the chain and peer lists empty while the status above them stayed current.
        const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
        // Peer targets load alongside: waiting for them afterwards doubled a timed-out refresh.
        const [statusResult, heartbeatsResult, peersResult, mempoolResult, peerTargetsResult] = await Promise.allSettled([
          this.getCheckedJson("/api/status", isObject),
          this.getCheckedJson(`/api/heartbeats?limit=${this.heartbeatPage.limit}`, Array.isArray),
          this.getCheckedJson("/api/peers", Array.isArray),
          this.getCheckedJson("/api/mempool", isObject),
          this.getCheckedJson("/api/peer-targets", Array.isArray),
        ]);
        // A failed secondary list is reported but does not slow polling; only the status does.
        failures = [statusResult, heartbeatsResult, peersResult, mempoolResult, peerTargetsResult]
          .filter((result) => result.status === "rejected")
          .map((result) => result.reason);
        if (peersResult.status === "fulfilled") {
          this.peers = peersResult.value;
          this.peersLoaded = true;
          this.peersError = false;
        } else {
          this.peersError = true;
        }
        if (mempoolResult.status === "fulfilled") this.mempool = mempoolResult.value;
        const heartbeats = heartbeatsResult.status === "fulfilled" ? heartbeatsResult.value : null;
        if (heartbeats) {
          const oldestFreshHeight = heartbeats[heartbeats.length - 1]?.height;
          const olderLoaded = oldestFreshHeight == null
            ? []
            : this.heartbeats.filter((block) => Number(block.height) < Number(oldestFreshHeight));
          this.heartbeats = [...heartbeats, ...olderLoaded];
          this.heartbeatPage.hasMore = this.heartbeats[this.heartbeats.length - 1]?.height > 0
            && heartbeats.length === this.heartbeatPage.limit;
        }
        if (peerTargetsResult.status === "fulfilled") this.peerTargets = peerTargetsResult.value;
        if (statusResult.status === "rejected") {
          failures = failures.filter((error) => error !== statusResult.reason);
          throw statusResult.reason;
        }
        const status = statusResult.value;
        this.sampleLocalHashrate(status);
        this.status = status;
        if (!this.p2pForm.busy && !this.p2pForm.dirty && status.p2p) {
          this.p2pForm.publicEnabled = status.p2p.public_enabled === true;
          this.p2pForm.bindPort = Number(status.p2p.bind_port || 9444);
          this.p2pForm.publicAddress = status.p2p.public_address || "";
        }
        if (!this.mining.busy) this.mining.workers = Number(status.heartbeat_mining_workers || 1);
        void this.reconcileSelectedTransaction();
        if (this.tab === "metrics") await this.refreshMetrics(true);
        if (status.wallet_public_key && this.tab === "wallet") {
          const activity = await this.getJson("/api/wallet/activity").catch(() => null);
          if (activity) this.applyActivityPage(activity, false);
          if (this.walletView === "utxos" && this.utxoPageKey !== this.utxoStatusKey()) await this.loadUtxos(false);
        }
        if (!this.mining.busy) {
          this.mining.message = status.heartbeat_mining_enabled
            ? "Heartbeat mining is on. The node searches for the next Heartbeat continuously."
            : "Heartbeat mining is off";
        }
        this.syncLedgerBlocks();
        if (status.ledger_mining_enabled
            && previousHeartbeatHead
            && previousHeartbeatHead !== this.heartbeats[0]?.block_hash) {
          await this.quoteContinuousTicket(true, false);
        }
        this.scheduleChainHeadSlide(
          "heartbeatRail",
          heartbeatRail,
          previousHeartbeatHead,
          this.heartbeats[0]?.block_hash,
        );
        this.scheduleChainHeadSlide(
          "ledgerRail",
          ledgerRail,
          previousLedgerHead,
          this.ledgerBlocks[0]?.block_hash,
        );
        if (this.reconcileSelectedBlock()) {
          this.selectedBlockDetail = null;
          await this.loadBlockDetail(this.selectedBlock, true);
        }
        if (this.tab === "chain") {
          this.selectDefaultChainBlock();
          this.$nextTick(() => this.ensureChainRailScrollable(this.chainView));
        }
        this.lastUpdated = new Date();
        this.pollFailures = 0;
      } catch (error) {
        this.pollFailures = Math.min(4, this.pollFailures + 1);
        failures.unshift(error);
      } finally {
        // Background refreshes stay quiet on screen, so every failure at least reaches the console.
        for (const error of failures) console.error("refresh failed", error);
        if (failures.length && !silent) this.notify(failures.map((error) => error.message).join("; "), "error");
        if (this.setupReady) this.syncLoginVisibility();
        else if (this.initialized) this.prepareSetup();
      }
    },

    chainRailState(reference) {
      const rail = this.$refs?.[reference];
      return {
        width: rail?.scrollWidth || 0,
        atStart: !rail || rail.scrollLeft <= 4,
      };
    },

    scheduleChainHeadSlide(reference, previous, previousHead, currentHead) {
      if (!previousHead || !currentHead || previousHead === currentHead || !previous.atStart) return;
      this.$nextTick(() => this.slideNewChainHead(reference, previous.width));
    },

    slideNewChainHead(reference, previousWidth) {
      const rail = this.$refs?.[reference];
      if (!rail || previousWidth === 0) return;
      const addedWidth = rail.scrollWidth - previousWidth;
      if (addedWidth <= 0) return;
      rail.scrollLeft = addedWidth;
      if (typeof rail.scrollTo === "function") {
        rail.scrollTo({ left: 0, behavior: "smooth" });
      } else {
        rail.scrollLeft = 0;
      }
    },

    // GETs give up after IUNA_REQUEST_TIMEOUT_MS. A node that accepts the connection but does not
    // answer, for example while it holds its chain lock, otherwise held the poll loop, and with it
    // the startup screen, until the page was reloaded. A caller's signal can still abort earlier.
    async readResponse(path, callerSignal) {
      const controller = typeof AbortController === "function" ? new AbortController() : null;
      let timedOut = false;
      const timer = controller
        ? setTimeout(() => { timedOut = true; controller.abort(); }, IUNA_REQUEST_TIMEOUT_MS)
        : null;
      if (controller && callerSignal) {
        if (callerSignal.aborted) controller.abort();
        else callerSignal.addEventListener("abort", () => controller.abort(), { once: true });
      }
      const signal = controller?.signal || callerSignal;
      try {
        const response = await fetch(path, { headers: this.headers(), cache: "no-store", signal });
        return { response, text: await response.text() };
      } catch (error) {
        if (timedOut) {
          throw new Error(`${path} did not answer within ${IUNA_REQUEST_TIMEOUT_MS / 1000} seconds`);
        }
        throw error;
      } finally {
        clearTimeout(timer);
      }
    },

    async getJson(path, signal) {
      const { response, text } = await this.readResponse(path, signal);
      const payload = this.parseResponseText(text);
      if (!response.ok) {
        const error = new Error(payload.error || `${path} returned ${response.status}`);
        error.status = response.status;
        throw error;
      }
      return payload;
    },

    async saveP2pSettings() {
      const bindPort = Number(this.p2pForm.bindPort);
      if (!Number.isInteger(bindPort) || bindPort < 1 || bindPort > 65535) {
        this.notify("P2P port must be between 1 and 65535", "error");
        return;
      }
      this.p2pForm.busy = true;
      try {
        const payload = await this.postJson("/api/settings/p2p", {
          public_enabled: this.p2pForm.publicEnabled === true,
          bind_port: bindPort,
          public_address: this.p2pForm.publicAddress.trim() || null,
        });
        this.status = { ...this.status, p2p: payload };
        this.p2pForm.publicEnabled = payload.public_enabled === true;
        this.p2pForm.bindPort = Number(payload.bind_port);
        this.p2pForm.publicAddress = payload.public_address || "";
        this.p2pForm.dirty = false;
        this.notify(payload.restart_required ? "Saved — restart iuna to apply the listener change" : "Public-node settings saved", "success");
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.p2pForm.busy = false;
      }
    },

    // Like getJson, but rejects a successful response whose body has the wrong shape, and logs
    // what arrived: an empty or non-JSON body still parses to an object in getJson.
    async getCheckedJson(path, valid) {
      const { response, text } = await this.readResponse(path);
      let payload;
      try { payload = text ? JSON.parse(text) : undefined; } catch { payload = undefined; }
      if (!response.ok) {
        const error = new Error(payload?.error || text || `${path} returned ${response.status}`);
        error.status = response.status;
        throw error;
      }
      if (!valid(payload)) {
        console.error("unexpected response", { path, status: response.status, body: text.slice(0, 300) });
        throw new Error(`${path} returned an unexpected response`);
      }
      return payload;
    },

    storedOperatorToken() {
      try { return sessionStorage.getItem("iunaOperatorToken") || ""; } catch { return ""; }
    },

    hasOperatorToken() {
      return this.tokenStored;
    },

    async saveOperatorToken() {
      const token = this.operatorToken.trim();
      if (token.length < 32) {
        this.notify("An operator token has at least 32 characters", "error");
        return;
      }
      try { sessionStorage.setItem("iunaOperatorToken", token); } catch {
        this.notify("Session storage is unavailable in this browser", "error");
        return;
      }
      this.operatorToken = "";
      this.tokenStored = true;
      this.notify("Operator token saved for this browser session");
      await this.refresh(true);
    },

    async forgetOperatorToken() {
      try { sessionStorage.removeItem("iunaOperatorToken"); } catch {}
      this.tokenStored = false;
      this.notify("Operator token removed");
      await this.refresh(true);
    },

    // Reads need no token, so only management POSTs carry it. Polling a port that something else
    // took over after the node stopped then never hands that listener the token.
    headers(management = false) {
      const headers = { Accept: "application/json" };
      if (!management) return headers;
      const token = this.storedOperatorToken();
      if (token) headers.Authorization = `Bearer ${token}`;
      headers["Content-Type"] = "application/json";
      return headers;
    },

    async responseJson(response) {
      return this.parseResponseText(await response.text());
    },

    parseResponseText(text) {
      if (!text) return {};
      try { return JSON.parse(text); } catch { return { error: text }; }
    },

    // The node may offer management, but in token mode this browser also needs the token.
    browserCanManage() {
      if (this.status.management_enabled !== true) return false;
      return this.status.management_requires_token !== true || this.tokenStored;
    },

    canManage() {
      return this.browserCanManage() && this.status.wallet_unlocked === true;
    },

    // The form only prepares the transfer; nothing is sent until the confirm dialog is accepted.
    reviewTransaction() {
      if (!this.canManage() || this.transfer.busy) return;
      try {
        const recipient = this.transfer.recipient.trim().toLowerCase();
        if (!/^[0-9a-f]{64}$/.test(recipient)) throw new Error("The recipient must be a 64-character address");
        const amount = this.parseAmount(this.transfer.amount, "Enter an amount");
        if (amount <= 0) throw new Error("Enter an amount above zero");
        const fee = this.parseAmount(this.transfer.fee);
        const spendable = Number(this.status.wallet_spendable_balance || 0);
        if (amount + fee > spendable) throw new Error("Amount plus fee exceeds the spendable balance");
        const limit = this.status.wallet_transfer_limit;
        if (limit && amount + fee > Number(limit.value)) throw new Error(this.transferLimitMessage(limit));
        this.pendingTransfer = { recipient, amount, fee, remaining: spendable - amount - fee };
        this.$nextTick(() => this.$refs.confirmTransfer?.focus());
      } catch (error) {
        this.notify(error.message, "error");
      }
    },

    async confirmTransaction() {
      const pending = this.pendingTransfer;
      if (!pending || !this.canManage() || this.transfer.busy) return;
      this.transfer.busy = true;
      try {
        const payload = await this.postJson("/api/ledger-transactions", {
          recipient: pending.recipient,
          amount: pending.amount,
          fee: pending.fee,
        });
        this.notify(`Ledger transaction ${this.short(payload.id)} submitted`);
        this.pendingTransfer = null;
        this.transfer.recipient = "";
        this.transfer.amount = "";
        await this.refresh(true);
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.transfer.busy = false;
      }
    },

    // A wallet with more spendable outputs than one transaction holds can send only its largest
    // ones at once, and spending many inputs raises the relay minimum fee.
    useMaximumAmount() {
      try {
        const entered = this.parseAmount(this.transfer.fee);
        const limit = this.status.wallet_transfer_limit;
        const fee = Math.max(entered, Number(limit?.minimum_fee || 0));
        const available = limit ? Number(limit.value) : Number(this.status.wallet_spendable_balance || 0);
        const maximum = available - fee;
        if (maximum <= 0) throw new Error("The spendable balance does not cover the fee");
        this.transfer.amount = this.amountLabel(maximum);
        if (fee > entered) {
          this.transfer.fee = this.amountLabel(fee);
          this.notify(`Fee raised to ${this.amountLabel(fee)} IUNA, the relay minimum for spending ${limit.inputs} outputs`);
        }
        if (available < Number(this.status.wallet_spendable_balance || 0)) {
          this.notify(this.transferLimitMessage(limit));
        }
      } catch (error) {
        this.notify(error.message, "error");
      }
    },

    transferLimitMessage(limit) {
      return `One transaction can spend at most ${limit.inputs} outputs, holding ${this.amountLabel(limit.value)} IUNA with the fee. `
        + "Send the rest in another transaction, or let continuous Ledger mining consolidate small outputs first.";
    },

    async setHeartbeatMining(enabled) {
      if (!this.canManage() || this.mining.busy) return;
      this.mining.busy = true;
      this.mining.message = enabled ? "Starting proof-of-work miner…" : "Stopping proof-of-work miner…";
      try {
        const payload = await this.postJson("/api/hash-mining", {
          enabled,
          workers: this.normalizedMiningWorkers(),
        });
        this.status.heartbeat_mining_enabled = payload.enabled;
        this.status.heartbeat_mining_workers = payload.workers;
        this.mining.workers = payload.workers;
        if (!payload.enabled) this.mining.hashrate = 0;
        this.mining.message = payload.enabled
          ? "Heartbeat mining is on. The node searches for the next Heartbeat continuously."
          : "Heartbeat mining is off";
        this.notify(payload.bootstrap_ticket_submitted
          ? "Heartbeat mining enabled; the public genesis funds created the first Proof of Expenditure ticket"
          : (payload.enabled ? "Heartbeat mining enabled" : "Heartbeat mining disabled"));
        await this.refresh(true);
      } catch (error) {
        this.mining.message = error.message;
        this.notify(error.message, "error");
      } finally {
        this.mining.busy = false;
      }
    },

    normalizedMiningWorkers() {
      const maximum = Number(this.status.heartbeat_mining_max_workers || 256);
      return Math.min(maximum, Math.max(1, Math.trunc(Number(this.mining.workers) || 1)));
    },

    async updateMiningWorkers() {
      if (!this.canManage() || this.mining.busy) return;
      this.mining.workers = this.normalizedMiningWorkers();
      this.mining.busy = true;
      try {
        const payload = await this.postJson("/api/hash-mining", {
          enabled: this.status.heartbeat_mining_enabled === true,
          workers: this.mining.workers,
        });
        this.status.heartbeat_mining_enabled = payload.enabled;
        this.status.heartbeat_mining_workers = payload.workers;
        this.mining.workers = payload.workers;
        this.notify(`Heartbeat mining will use ${payload.workers} worker${payload.workers === 1 ? "" : "s"}`);
      } catch (error) {
        this.notify(error.message, "error");
        await this.refresh(true);
      } finally {
        this.mining.busy = false;
      }
    },

    async setLedgerMining(enabled) {
      if (!this.canManage() || this.ticket.continuousBusy) return;
      this.ticket.continuousBusy = true;
      try {
        const payload = await this.postJson("/api/ledger-mining", { enabled });
        this.status.ledger_mining_enabled = payload.enabled;
        if (payload.enabled) await this.quoteContinuousTicket(true, false);
        this.notify(payload.enabled ? "Continuous Ledger mining enabled" : "Continuous Ledger mining disabled");
        await this.refresh(true);
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.ticket.continuousBusy = false;
      }
    },

    async quoteTicket(silent = false) {
      const duration = Math.max(1, Math.trunc(Number(this.ticket.duration) || 1));
      this.ticket.duration = duration;
      try {
        this.ticket.quote = await this.getJson(`/api/mining-ticket-quote?duration=${duration}`);
        if (duration === 10) this.ticket.continuousQuote = this.ticket.quote;
        if (!this.ticket.budget) {
          this.ticket.budget = this.amountLabel(this.ticket.quote.recommended_budget);
        }
      } catch (error) {
        this.ticket.quote = null;
        if (!silent) this.notify(error.message, "error");
      }
    },

    async quoteContinuousTicket(silent = false, useManualQuote = true) {
      if (useManualQuote && Number(this.ticket.quote?.duration) === 10) {
        this.ticket.continuousQuote = this.ticket.quote;
        return;
      }
      try {
        this.ticket.continuousQuote = await this.getJson("/api/mining-ticket-quote?duration=10");
      } catch (error) {
        this.ticket.continuousQuote = null;
        if (!silent) this.notify(error.message, "error");
      }
    },

    ledgerMiningMessage() {
      if (this.ticket.continuousBusy) return "Updating continuous Ledger mining…";
      return this.status.ledger_mining_enabled
        ? "Continuous Ledger mining is on. The node submits a ticket every 10 Heartbeats."
        : "Continuous Ledger mining is off. Submit a ticket manually above.";
    },

    ledgerOtherTicketPower() {
      const value = Number(this.ticket.continuousQuote?.other_active_ticket_weight);
      return Number.isSafeInteger(value) && value >= 0 ? value : null;
    },

    ledgerOtherTicketPowerLabel() {
      const value = this.ledgerOtherTicketPower();
      return value == null ? "…" : this.amountLabel(value);
    },

    ledgerNextTicketLabel() {
      const lastHeight = this.status.ledger_last_ticket_height;
      if (lastHeight == null) return "As soon as possible";
      const nextHeight = Number(lastHeight) + 10;
      return Number(this.status.heartbeat_height || 0) >= nextHeight ? "Due now" : `Heartbeat #${nextHeight}`;
    },

    async submitTicket() {
      if (!this.canManage() || this.ticket.busy) return;
      this.ticket.busy = true;
      try {
        const payload = await this.postJson("/api/mining-tickets", {
          budget: this.parseAmount(this.ticket.budget, "Enter a ticket budget"),
          duration: Math.max(1, Math.trunc(Number(this.ticket.duration) || 1)),
          fee: this.parseAmount(this.ticket.fee),
        });
        this.notify(`Mining ticket ${this.short(payload.id)} submitted`);
        await this.refresh(true);
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.ticket.busy = false;
      }
    },

    async postJson(path, body) {
      const response = await fetch(path, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
      const payload = await this.responseJson(response);
      if (!response.ok) throw new Error(payload.error || `${path} returned ${response.status}`);
      return payload;
    },

    parseAmount(value, message = "Enter a valid amount") {
      const text = String(value ?? "").trim().replace(",", ".");
      const match = text.match(/^(\d+)(?:\.(\d{0,8}))?$/);
      if (!match) throw new Error(message);
      const amount = Number(match[1]) * 100_000_000 + Number((match[2] || "").padEnd(8, "0"));
      if (!Number.isSafeInteger(amount) || amount < 0) throw new Error(message);
      return amount;
    },

    amountLabel(value) {
      const amount = Math.max(0, Math.round(Number(value) || 0));
      const whole = Math.floor(amount / 100_000_000);
      const fraction = String(amount % 100_000_000).padStart(8, "0").replace(/0+$/, "");
      return fraction ? `${whole}.${fraction}` : String(whole);
    },

    pendingCount() {
      return Number(this.status.pending_ledger_transactions || 0) + Number(this.status.pending_mining_tickets || 0);
    },

    networkLabel() {
      if (!this.status.network) return "Connecting";
      if (!this.peersLoaded) return this.peersError ? "Unavailable" : "Loading";
      if (this.syncing()) return this.syncingLabel();
      return this.peers.length ? "Connected" : "Isolated";
    },

    // Behind a peer by height, or downloading blocks from one. The second also covers a side
    // branch: after a restart this node may have mined its own branch as high as the network's,
    // and the network's branch only takes over once it is fully fetched and outweighs it.
    syncing() {
      return Boolean(this.syncTarget()) || this.peers.some((peer) => peer.syncing);
    },

    syncingLabel() {
      return this.syncTarget() ? `Syncing ${this.syncPercent()}%` : "Syncing";
    },

    // Rounded down, so 100% only shows once nothing is left to fetch.
    syncPercent() {
      const target = this.syncTarget();
      if (!target) return 100;
      return Math.min(99, Math.floor((100 * Number(this.status.heartbeat_height || 0)) / target));
    },

    networkHeight() {
      const best = Math.max(0, ...this.peers.map((peer) => Number(peer.heartbeat_height || 0)));
      return this.peers.length ? Math.max(best, Number(this.status.heartbeat_height || 0)) : null;
    },

    syncLabel() {
      if (!this.status.network) return "Waiting";
      if (!this.peersLoaded) return this.peersError ? "Unavailable" : "Loading";
      if (!this.peers.length) return "No peers";
      return this.syncing() ? this.syncingLabel() : "Up to date";
    },

    networkTileDetail() {
      if (!this.status.network) return "Waiting for the local node";
      if (!this.peersLoaded) return this.peersError ? "Peer list unavailable" : "Loading peers";
      if (!this.peers.length) return "No peers connected";
      const peers = `${this.peers.length} peer${this.peers.length === 1 ? "" : "s"}`;
      return this.syncing() ? peers : `${peers} · up to date`;
    },

    // Highest Heartbeat a connected peer announced, when it is ahead of this node.
    syncTarget() {
      const local = Number(this.status.heartbeat_height || 0);
      const best = Math.max(0, ...this.peers.map((peer) => Number(peer.heartbeat_height || 0)));
      return best > local + 1 ? best : null;
    },

    networkDetail() {
      if (!this.status.network) return "Waiting for the local node to respond.";
      if (!this.peersLoaded) {
        return this.peersError
          ? "The local node did not return its peer list. Retrying in the background."
          : "Loading the peer list from the local node.";
      }
      if (!this.peers.length) return "No peers yet. The node keeps retrying its configured peers; you can still use it on your own.";
      if (this.syncTarget()) return `Synchronizing Heartbeat ${Number(this.status.heartbeat_height || 0)} of ${this.syncTarget()}. Blocks appear as they arrive.`;
      if (this.syncing()) return "Fetching blocks from the network. Blocks appear as they arrive.";
      return `${this.peers.length} peer${this.peers.length === 1 ? "" : "s"} connected. Blocks and transactions arrive automatically.`;
    },

    // Median interval over the loaded Heartbeats, to compare against the 60-second target. The
    // median ignores outages (a node that was off for hours); genesis is excluded because its
    // pinned timestamp predates the network's first mined block.
    averageHeartbeatSeconds() {
      const cache = this.chartCache();
      if (cache.medianInterval !== undefined) return cache.medianInterval;
      const intervals = this.chartSeries("interval").map((point) => point.y).sort((a, b) => a - b);
      if (!intervals.length) {
        cache.medianInterval = null;
        return null;
      }
      const middle = Math.floor(intervals.length / 2);
      cache.medianInterval = intervals.length % 2 ? intervals[middle] : (intervals[middle - 1] + intervals[middle]) / 2;
      return cache.medianInterval;
    },

    averageHeartbeatLabel() {
      const seconds = this.averageHeartbeatSeconds();
      return seconds == null ? "-" : `${seconds.toFixed(seconds < 10 ? 1 : 0)} s`;
    },

    // Bar width relative to twice the target, so the 60-second target sits in the middle.
    heartbeatIntervalShare() {
      const seconds = this.averageHeartbeatSeconds();
      return seconds == null ? 0 : Math.min(100, (seconds / 120) * 100);
    },

    // Runs once per opening of the Chain screen, so closing the details does not reopen them.
    selectDefaultChainBlock() {
      if (!this.chainDefaultSelectionPending || this.tab !== "chain") return;
      if (this.selectedBlock) {
        this.chainDefaultSelectionPending = false;
        return;
      }
      const latest = this.ledgerBlocks[0];
      if (!latest) return;
      this.chainDefaultSelectionPending = false;
      this.selectBlock(latest, "ledger");
    },

    async selectBlock(block, chain) {
      this.chainDefaultSelectionPending = false;
      const sameBlock = this.selectedBlock?.block_hash === block.block_hash;
      if (sameBlock && this.selectedBlockChain === chain) {
        this.clearSelectedBlock();
        return;
      }
      this.selectedBlock = block;
      this.selectedBlockChain = chain;
      const blocks = chain === "ledger" ? this.ledgerBlocks : this.heartbeats;
      this.selectedBlockFollowsLatest = blocks[0]?.block_hash === block.block_hash;
      if (sameBlock) return;
      this.selectedBlockDetail = null;
      await this.loadBlockDetail(block);
    },

    clearSelectedBlock() {
      this.blockDetailRequestId += 1;
      this.selectedBlock = null;
      this.selectedBlockDetail = null;
      this.selectedBlockChain = null;
      this.selectedBlockFollowsLatest = false;
      this.blockDetailBusy = false;
    },

    reconcileSelectedBlock() {
      if (!this.selectedBlock) return false;
      const previousHash = this.selectedBlock.block_hash;
      if (this.selectedBlockFollowsLatest) {
        const blocks = this.selectedBlockChain === "ledger" ? this.ledgerBlocks : this.heartbeats;
        if (blocks.length) this.selectedBlock = blocks[0];
      } else {
        // Refresh the selected summary when it is still in the loaded window. If an old block has
        // moved beyond that window, retain its selection and already-loaded details.
        const blocks = this.selectedBlockChain === "ledger" ? this.ledgerBlocks : this.heartbeats;
        this.selectedBlock = blocks.find((block) => block.block_hash === previousHash)
          || this.selectedBlock;
      }
      return this.selectedBlock.block_hash !== previousHash;
    },

    async loadBlockDetail(block, silent = false) {
      const requestId = ++this.blockDetailRequestId;
      this.blockDetailBusy = true;
      try {
        const detail = await this.getJson(`/api/blocks/${block.block_hash}`);
        if (requestId === this.blockDetailRequestId
            && this.selectedBlock?.block_hash === block.block_hash) {
          this.selectedBlockDetail = detail;
        }
      } catch (error) {
        if (!silent) this.notify(error.message, "error");
      } finally {
        if (requestId === this.blockDetailRequestId) this.blockDetailBusy = false;
      }
    },

    blockSelected(block) {
      return this.selectedBlock?.block_hash === block.block_hash;
    },

    setChainView(chain) {
      if (!["heartbeat", "ledger"].includes(chain)) return;
      this.chainView = chain;
      this.$nextTick(() => this.ensureChainRailScrollable(chain));
    },

    syncLedgerBlocks() {
      this.ledgerBlocks = this.heartbeats
        .filter((block) => block.has_ledger_block)
        .map((block, index) => ({
          ...block,
          ledger_height: Number(block.ledger_blocks) || Math.max(1, Number(this.status.ledger_blocks || 0) - index),
        }));
    },

    async loadMoreHeartbeats(chain = this.chainView) {
      if (this.heartbeatPage.busy || !this.heartbeatPage.hasMore || !this.heartbeats.length) return;
      this.heartbeatPage.busy = true;
      const previousLedgerCount = this.ledgerBlocks.length;
      try {
        do {
          const beforeHeight = Number(this.heartbeats[this.heartbeats.length - 1]?.height);
          if (!Number.isFinite(beforeHeight) || beforeHeight <= 0) {
            this.heartbeatPage.hasMore = false;
            break;
          }
          const page = await this.getJson(`/api/heartbeats?limit=${this.heartbeatPage.limit}&before_height=${beforeHeight}`);
          const known = new Set(this.heartbeats.map((block) => block.block_hash));
          const older = page.filter((block) => !known.has(block.block_hash));
          // Merged, not pushed: a window load may have replaced the list while this page loaded.
          this.mergeHeartbeats(older);
          this.heartbeatPage.hasMore = page.length === this.heartbeatPage.limit
            && Number(page[page.length - 1]?.height) > 0;
          if (!page.length || !older.length) this.heartbeatPage.hasMore = false;
        } while (chain === "ledger"
          && this.ledgerBlocks.length === previousLedgerCount
          && this.heartbeatPage.hasMore);
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.heartbeatPage.busy = false;
      }
    },

    handleChainScroll(chain, event) {
      const rail = event?.currentTarget;
      if (!rail || rail.scrollLeft + rail.clientWidth < rail.scrollWidth - 180) return;
      this.loadMoreHeartbeats(chain);
    },

    async ensureChainRailScrollable(chain) {
      if (this.tab !== "chain" || this.chainView !== chain || this.heartbeatPage.busy || !this.heartbeatPage.hasMore) return;
      const reference = chain === "ledger" ? "ledgerRail" : "heartbeatRail";
      const rail = this.$refs?.[reference];
      if (!rail || rail.clientWidth <= 0) return;
      const needsInitialLedgerWindow = chain === "ledger" && this.ledgerBlocks.length < 20;
      if (!needsInitialLedgerWindow && rail.scrollWidth > rail.clientWidth + 4) return;
      const previousHeartbeatCount = this.heartbeats.length;
      await this.loadMoreHeartbeats(chain);
      if (this.heartbeats.length === previousHeartbeatCount) return;
      this.$nextTick(() => this.ensureChainRailScrollable(chain));
    },

    minerLabel(publicKey) {
      const key = String(publicKey || "").toLowerCase();
      if (!/^[0-9a-f]{64}$/.test(key)) return "-";
      const mine = key === String(this.status.wallet_public_key || "").toLowerCase();
      return `${key.slice(-8)}${mine ? " (me)" : ""}`;
    },

    // Blocks name miners by public key, but rewards pay, and contacts store, the address.
    blockPartyLabel(publicKey, address) {
      if (!/^[0-9a-f]{64}$/.test(String(address || ""))) return "-";
      const mine = String(publicKey || "").toLowerCase() === String(this.status.wallet_public_key || "").toLowerCase();
      const name = this.contactName(address) || (mine ? "Me" : "");
      return name ? `${name} · ${address}` : address;
    },

    mempoolItems() {
      return [
        ...(this.mempool?.ledger_transactions || []).map((item) => ({
          ...item,
          kind: "Ledger transaction",
          amount: item.total_output,
        })),
        ...(this.mempool?.mining_tickets || []).map((item) => ({
          ...item,
          kind: "Mining ticket",
          amount: item.budget,
        })),
      ];
    },

    openTransactionModal(item, source = "Mempool") {
      this.transactionModalTrigger = globalThis.document?.activeElement || null;
      this.selectedTransaction = { item, source };
      this.$nextTick(() => this.$refs.transactionModal?.focus());
    },

    // The strip only holds summaries; inputs, outputs and signatures load with the modal.
    async openMempoolModal(item) {
      this.openTransactionModal(item);
      this.selectedTransaction = { ...this.selectedTransaction, loading: true };
      const stillSelected = () => this.selectedTransaction?.item?.id === item.id;
      try {
        const detail = await this.getJson(`/api/mempool/${item.id}`);
        if (stillSelected()) {
          this.selectedTransaction = {
            ...this.selectedTransaction,
            item: { ...item, ...detail, amount: detail.total_output ?? detail.budget ?? item.amount },
            loading: false,
          };
        }
      } catch (error) {
        if (stillSelected()) {
          this.selectedTransaction = {
            ...this.selectedTransaction,
            source: error.status === 404 ? "No longer in mempool" : "Mempool",
            loading: false,
            detailMissing: true,
          };
        }
      }
    },

    // The label follows the loaded detail: `selectedBlock` can already point at a newer head.
    selectedBlockSource() {
      return `Heartbeat ${this.selectedBlockDetail?.height ?? this.selectedBlock?.height ?? "-"}`;
    },

    openHeartbeatTicketModal(ticket) {
      this.openTransactionModal({
        ...ticket,
        kind: "Mining ticket",
        amount: ticket.budget,
      }, this.selectedBlockSource());
    },

    // The link shows the loaded detail, and `selectedBlock` can already point at a newer head.
    openBlockRewardModal(kind) {
      const detail = this.selectedBlockDetail;
      if (!detail) return;
      return this.openRewardModal(detail.block_hash, kind, detail.rewards?.[kind]?.total, this.selectedBlockSource());
    },

    // A reward has no transaction of its own: the node lists the fees and payouts it collects.
    async openRewardModal(blockHash, kind, amount, source) {
      const label = kind === "heartbeat" ? "Heartbeat reward" : "Ledger reward";
      this.openTransactionModal({ id: blockHash, kind: label, amount, reward: true }, source);
      this.selectedTransaction = { ...this.selectedTransaction, loading: true };
      const stillSelected = () => this.selectedTransaction?.item?.id === blockHash
        && this.selectedTransaction?.item?.kind === label;
      let item = null;
      try {
        item = this.rewardItem(await this.getJson(`/api/rewards/${blockHash}`), kind);
      } catch {
        item = null;
      }
      if (!stillSelected()) return;
      this.selectedTransaction = item
        ? { ...this.selectedTransaction, item, loading: false }
        : { ...this.selectedTransaction, loading: false, detailMissing: true };
    },

    rewardItem(breakdown, kind) {
      const reward = breakdown?.[kind];
      if (!reward) return null;
      const label = kind === "heartbeat" ? "Heartbeat reward" : "Ledger reward";
      return {
        id: breakdown.block_hash,
        kind: label,
        amount: reward.total,
        reward: true,
        inputs: reward.inputs.map((input, index) => ({
          label: IUNA_REWARD_INPUTS[input.source]?.label || input.source,
          note: input.id ? "" : IUNA_REWARD_INPUTS[input.source]?.note,
          ref: input.id,
          value: input.value,
          key: `${input.source}:${input.id ?? index}`,
        })),
        // A node from before reward outpoints sends no outputs; its reward is then one output.
        outputs: (reward.outputs || [{ owner: reward.owner, value: reward.total }]).map((output) => ({
          label: IUNA_REWARD_OUTPUTS[output.source] || label,
          kind: "output",
          owner: output.owner,
          ref: output.transaction_id == null ? "" : `${output.transaction_id}:${output.output_index}`,
          value: output.value,
        })),
      };
    },

    openBlockTransactionModal(transaction) {
      this.openTransactionModal({
        ...transaction,
        kind: "Ledger transaction",
        amount: transaction.total_output,
      }, this.selectedBlockSource());
    },

    // The block detail at a height, or null when it is unavailable.
    async heartbeatDetailAt(height) {
      try {
        const [summary] = await this.getJson(`/api/heartbeats?limit=1&before_height=${Number(height) + 1}`);
        if (Number(summary?.height) !== Number(height)) return null;
        return await this.getJson(`/api/blocks/${summary.block_hash}`);
      } catch {
        return null;
      }
    },

    // An activity row only holds a summary; the modal loads its block for the details.
    async openActivityModal(activity) {
      const { id, amount, heartbeat_height: height } = activity;
      // A reward's ID is the hash of the block that paid it.
      const rewardKind = { heartbeat_reward: "heartbeat", ledger_reward: "ledger" }[activity.kind];
      if (rewardKind) {
        await this.openRewardModal(id, rewardKind, amount, `Heartbeat ${height}`);
        return;
      }
      const isTicket = activity.kind === "ticket";
      const kind = isTicket ? "Mining ticket" : "Ledger transaction";
      if (activity.pending) {
        await this.openMempoolModal({ id, kind, amount });
        return;
      }
      this.openTransactionModal({ id, kind, amount }, `Heartbeat ${height}`);
      this.selectedTransaction = { ...this.selectedTransaction, loading: true };
      const stillSelected = () => this.selectedTransaction?.item?.id === id;
      const detail = await this.heartbeatDetailAt(height);
      const candidates = isTicket ? detail?.tickets : detail?.ledger?.transactions;
      const found = candidates?.find((candidate) => candidate.id === id);
      const item = found ? { ...found, kind, amount: isTicket ? found.budget : found.total_output } : null;
      if (!stillSelected()) return;
      this.selectedTransaction = item
        ? { ...this.selectedTransaction, item, loading: false }
        : { ...this.selectedTransaction, loading: false, detailMissing: true };
    },

    closeTransactionModal() {
      this.selectedTransaction = null;
      const trigger = this.transactionModalTrigger;
      this.transactionModalTrigger = null;
      this.$nextTick(() => trigger?.focus?.());
    },

    // The open modal shows a snapshot, so say when the item stopped being pending. The strip is a
    // capped preview, so only the node can confirm that an item missing from it is gone.
    async reconcileSelectedTransaction() {
      const selected = this.selectedTransaction;
      if (selected?.source !== "Mempool") return;
      const { id, kind } = selected.item;
      const inPreview = this.mempoolItems().some((item) => item.kind === kind && item.id === id);
      if (inPreview && !selected.detailMissing) return;
      const stillShown = () => this.selectedTransaction?.item?.id === id
        && this.selectedTransaction?.source === "Mempool";
      try {
        const detail = await this.getJson(`/api/mempool/${id}`);
        if (stillShown() && this.selectedTransaction.detailMissing) {
          this.selectedTransaction = {
            ...this.selectedTransaction,
            item: { ...this.selectedTransaction.item, ...detail },
            detailMissing: false,
          };
        }
      } catch (error) {
        if (error.status === 404 && stillShown()) {
          this.selectedTransaction = { ...this.selectedTransaction, source: "No longer in mempool" };
        }
      }
    },

    trapTransactionModalFocus(event) {
      const modal = this.$refs.transactionModal;
      const focusable = [...(modal?.querySelectorAll('button:not([disabled]), a[href], [tabindex="0"]') || [])];
      if (!focusable.length) {
        event.preventDefault();
        return;
      }
      const active = globalThis.document?.activeElement;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (active === first || active === modal)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    },

    transactionModalInputs() {
      return this.selectedTransaction?.item?.inputs || [];
    },

    transactionModalOutputs() {
      const item = this.selectedTransaction?.item;
      if (!item) return [];
      if (item.reward) return item.outputs || [];
      // An activity summary has no ticket terms until its detail loads.
      if (item.kind === "Mining ticket" && item.lifecycle_heartbeats == null) return [];
      if (item.kind === "Ledger transaction") {
        return [
          ...(item.outputs || []).map((output, index) => ({
            ...output,
            label: `Output ${index + 1}`,
            kind: "output",
          })),
          ...this.transactionFeeOutputs(item.fee_split),
        ];
      }
      const slice = Math.floor(Number(item.budget) / Number(item.lifecycle_heartbeats));
      return [
        {
          label: "Ticket budget",
          kind: "ticket",
          status: "locked",
          note: `Paid to Heartbeat miners in ${item.lifecycle_heartbeats} slices of about ${this.amountLabel(slice)} IUNA, starting at the inclusion Heartbeat. Not a spendable output.`,
          value: item.budget,
        },
        ...(item.change ? [{ ...item.change, label: "Change", kind: "change" }] : []),
        ...(item.fee == null ? [] : [{
          label: "Inclusion fee",
          kind: "fee",
          owner: item.fee_recipient,
          note: item.fee_recipient ? "" : "Goes to the miner of the inclusion Heartbeat, known once mined.",
          value: item.fee,
        }]),
      ];
    },

    transactionFeeOutputs(split) {
      if (!split) return [];
      const output = (label, share, recipient) => ({
        label,
        kind: "fee",
        value: share.value,
        owner: share.owner,
        note: share.owner ? "" : `Goes to the ${recipient}, known once mined. Rounding across the block can move one unit.`,
      });
      return [
        output("Heartbeat miner fee", split.heartbeat_miner, "Heartbeat miner"),
        output("Ledger winner fee", split.ledger_winner, "Ledger winner"),
      ].filter((fee) => fee.value > 0);
    },

    // Null when an amount is unknown, so the modal never presents a partial sum as a total.
    transactionModalTotals() {
      const sum = (items) => items.reduce(
        (total, { value }) => (total == null || value == null ? null : total + Number(value)),
        0,
      );
      const partial = this.selectedTransaction?.loading || this.selectedTransaction?.detailMissing;
      const inputs = this.transactionModalInputs();
      // A transaction always spends something, so no inputs means unknown. A loaded reward of
      // zero collects nothing, so its total in is zero, as in the browser wallet.
      const known = inputs.length || this.selectedTransaction?.item?.reward;
      const totalIn = partial || !known ? null : sum(inputs);
      const totalOut = partial ? null : sum(this.transactionModalOutputs());
      return { in: totalIn, out: totalOut, balanced: totalIn != null && totalIn === totalOut };
    },

    difficultyLabel() {
      return this.status.difficulty || "-";
    },

    // The node exposes a cumulative attempted-nonce counter. Sampling its delta keeps timing and
    // UI concerns outside the hot mining loop while reporting this machine's real throughput.
    sampleLocalHashrate(status, sampledAt = Date.now()) {
      const hashes = Math.max(0, Number(status.heartbeat_hashes || 0));
      const previousHashes = this.mining.hashes;
      const previousSample = this.mining.sampledAt;
      if (!status.heartbeat_mining_enabled) {
        this.mining.hashrate = 0;
      } else if (previousHashes != null && previousSample != null && hashes >= previousHashes && sampledAt > previousSample) {
        this.mining.hashrate = ((hashes - previousHashes) * 1000) / (sampledAt - previousSample);
      }
      this.mining.hashes = hashes;
      this.mining.sampledAt = sampledAt;
    },

    powHashrate() {
      return this.mining.hashrate;
    },

    hashrateLabel(value) {
      if (value == null || !Number.isFinite(Number(value))) return "—";
      const rate = Math.max(0, Number(value));
      if (rate === 0) return "0 H/s";
      const units = ["H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s"];
      let scaled = rate;
      let unit = 0;
      while (scaled >= 1000 && unit < units.length - 1) {
        scaled /= 1000;
        unit += 1;
      }
      const digits = scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2;
      return `${scaled.toFixed(digits)} ${units[unit]}`;
    },

    powHashrateLabel() {
      return this.powHashrate() == null ? "Measuring…" : this.hashrateLabel(this.powHashrate());
    },

    niceMeterMaximum(value, minimum) {
      const safe = Math.max(Number(value) || 0, minimum);
      const exponent = 10 ** Math.floor(Math.log10(safe));
      const normalized = safe / exponent;
      const multiplier = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
      return multiplier * exponent;
    },

    powHashrateScaleMax() {
      return this.niceMeterMaximum(this.powHashrate(), 1_000_000);
    },

    powHashrateShare() {
      const rate = this.powHashrate();
      return rate == null ? 0 : Math.min(100, (rate / this.powHashrateScaleMax()) * 100);
    },

    powHashrateAriaLabel() {
      const label = this.powHashrateLabel();
      return label === "Measuring…" ? "Measuring this node's hashrate" : `This node's hashrate ${label}`;
    },

    // The node sums the active ticket weight belonging to its own wallet. Ticket weight is budget
    // divided across active Heartbeats, making this the local lottery-power analogue of hashrate.
    ledgerPower() {
      return Math.max(0, Number(this.status.ledger_ticket_power || 0));
    },

    ledgerPowerLabel() {
      return `${this.amountLabel(this.ledgerPower())} IUNA / Heartbeat`;
    },

    ledgerPowerScaleMax() {
      return this.niceMeterMaximum(this.ledgerPower(), 1_000_000);
    },

    ledgerPowerShare() {
      return Math.min(100, (this.ledgerPower() / this.ledgerPowerScaleMax()) * 100);
    },

    ledgerPowerAriaLabel() {
      return `This node's active ticket power ${this.ledgerPowerLabel()}`;
    },

    // Share of loaded Heartbeats carrying a Ledger block; the target is 1 in 10, drawn at 50%.
    ledgerShare() {
      const rows = this.metricsRows.length ? this.metricsRows : this.heartbeats;
      if (!rows.length) return 0;
      const ledgerCount = rows.filter((block) => block.has_ledger_block).length;
      return Math.min(100, (ledgerCount / rows.length) * 500);
    },

    ledgerShareLabel() {
      const rows = this.metricsRows.length ? this.metricsRows : this.heartbeats;
      if (!rows.length) return "-";
      const ledgerCount = rows.filter((block) => block.has_ledger_block).length;
      return `${((ledgerCount / rows.length) * 100).toFixed(1)}%`;
    },

    // good / warn / bad / idle state for each dashboard tile.
    dashboardState(tile) {
      const status = this.status;
      if (!status.network) return "warn";
      if (tile === "heartbeatMining") return status.heartbeat_mining_enabled ? "good" : "idle";
      if (tile === "ledgerMining") return status.ledger_mining_enabled || this.ledgerPower() > 0 ? "good" : "idle";
      if (tile === "mempool") return this.pendingCount() ? "good" : "idle";
      if (tile === "p2p" && !this.peersLoaded) return this.peersError ? "warn" : "idle";
      if (tile === "p2p") return this.peers.length && !this.syncing() ? "good" : "warn";
      // An old tip while catching up is expected, not a stalled chain.
      if (this.syncing()) return "warn";
      if (tile === "ledger") return Number(status.ledger_blocks || 0) > 0 ? "good" : "idle";
      if (tile === "heartbeat") {
        const tip = this.heartbeats[0];
        if (!tip || Number(tip.height) === 0) return "warn";
        const age = Date.now() / 1000 - Number(tip.timestamp);
        return age > 600 ? "bad" : (age > 180 ? "warn" : "good");
      }
      return "good";
    },

    blockAgeLabel(block, emptyLabel = "No blocks") {
      if (!block || Number(block.height) === 0) return emptyLabel;
      const seconds = Math.max(0, Math.round(Date.now() / 1000 - Number(block.timestamp)));
      if (seconds < 60) return `${seconds} s ago`;
      if (seconds < 3600) return `${Math.floor(seconds / 60)} min ago`;
      return `${Math.floor(seconds / 3600)} h ago`;
    },

    accentOptions() {
      return IUNA_ACCENTS;
    },

    setAccent(accent) {
      if (!IUNA_ACCENTS.some((option) => option.id === accent)) return;
      this.accent = accent;
      iunaApplyAccent(accent);
      try { localStorage.setItem("iunaAccent", accent); } catch {}
    },

    // "⌘K" on macOS, "Ctrl K" elsewhere, matching shortcutModifier.
    shortcutLabel(key) {
      return /Mac|iPhone|iPad/.test(navigator.platform || "") ? `⌘${key}` : `Ctrl ${key}`;
    },

    openPalette() {
      this.palette.trigger = globalThis.document?.activeElement || null;
      this.shortcutKeyHeld = false;
      this.palette.open = true;
      this.palette.query = "";
      this.paletteSearch();
      // Alpine applies x-show in an animation frame, after $nextTick, so focus one frame later.
      const nextFrame = globalThis.requestAnimationFrame || ((callback) => setTimeout(callback, 16));
      this.$nextTick(() => nextFrame(() => this.$refs.paletteInput?.focus()));
    },

    // Focus goes back to where it was right away, so a dialog a result opens records that element,
    // not the hidden palette input, as the place to return to.
    closePalette(restoreFocus = true) {
      const trigger = this.palette.trigger;
      this.palette.open = false;
      this.palette.busy = false;
      this.palette.requestId += 1;
      this.palette.trigger = null;
      if (restoreFocus) trigger?.focus?.();
    },

    paletteMove(delta) {
      const count = this.palette.results.length;
      if (!count) return;
      this.palette.active = (this.palette.active + delta + count) % count;
      this.$nextTick(() => globalThis.document?.getElementById(`palette-result-${this.palette.active}`)?.scrollIntoView?.({ block: "nearest" }));
    },

    async paletteRun(index) {
      const result = this.palette.results[index];
      if (!result) return;
      this.closePalette();
      this.paletteNavigation += 1;
      try {
        await result.run();
      } catch (error) {
        this.notify(error.message, "error");
      }
    },

    // Local matches show at once; a full 64-character hash also asks the node whether it names a
    // block or a mempool item. Heights resolve only when chosen, so typing costs no requests.
    async paletteSearch() {
      const query = this.palette.query.trim();
      const requestId = ++this.palette.requestId;
      this.palette.results = this.paletteLocalResults(query);
      this.palette.active = 0;
      const hex = query.toLowerCase().replace(/^0x/, "");
      if (!/^[0-9a-f]{64}$/.test(hex)) {
        this.palette.busy = false;
        return;
      }
      this.palette.busy = true;
      const found = await this.paletteHashResults(hex);
      if (requestId !== this.palette.requestId) return;
      this.palette.busy = false;
      const seen = new Set(found.map((result) => result.key));
      this.palette.results = [...found, ...this.palette.results.filter((result) => !seen.has(result.key))];
    },

    paletteLocalResults(query) {
      const text = query.toLowerCase();
      const results = [];
      const add = (result) => { if (!results.some((item) => item.key === result.key)) results.push(result); };
      const number = /^(?:(h|heartbeat|l|ledger)\s*)?#?(\d{1,12})$/.exec(text);
      if (number) {
        const value = Number(number[2]);
        const wantsLedger = number[1]?.startsWith("l");
        const wantsHeartbeat = number[1]?.startsWith("h");
        if (!wantsLedger && value <= Number(this.status.heartbeat_height || 0)) {
          add({ key: `heartbeat:${value}`, kind: "Heartbeat", title: `Heartbeat #${value}`, detail: "Open in Chain", run: () => this.openHeartbeatHeight(value) });
        }
        if (!wantsHeartbeat && value >= 1 && value <= Number(this.status.ledger_blocks || 0)) {
          add({ key: `ledger:${value}`, kind: "Ledger", title: `Ledger block #${value}`, detail: "Open in Chain", run: () => this.openLedgerHeight(value) });
        }
      }
      const hex = text.replace(/^0x/, "");
      if (/^[0-9a-f]{6,64}$/.test(hex)) {
        for (const block of this.heartbeats) {
          if (block.block_hash.startsWith(hex)) {
            add({ key: `heartbeat:${block.height}`, kind: "Heartbeat", title: `Heartbeat #${block.height}`, detail: this.short(block.block_hash), run: () => this.openChainBlock(block, "heartbeat") });
          }
        }
        for (const item of this.mempoolItems()) {
          if (item.id.startsWith(hex)) {
            add({ key: `pending:${item.id}`, kind: "Pending", title: `${item.kind} · ${this.amountLabel(item.amount)} IUNA`, detail: this.short(item.id), run: () => this.openMempoolModal(item) });
          }
        }
        for (const item of this.activityItems?.() || []) {
          if (String(item.id).startsWith(hex)) {
            add({ key: `activity:${item.pending}:${item.kind}:${item.id}`, kind: "Wallet", title: `${this.activityLabel(item)} · ${this.activitySign(item)}${this.amountLabel(item.amount)} IUNA`, detail: this.short(item.id), run: () => this.openActivityModal(item) });
          }
        }
      }
      if (text && !number) {
        for (const contact of this.contacts) {
          if (contact.name.toLowerCase().includes(text) || (hex.length >= 6 && contact.address.startsWith(hex))) {
            add({ key: `contact:${contact.address}`, kind: "Contact", title: contact.name, detail: `Send to ${this.short(contact.address)}`, run: () => this.useRecipient(contact.address) });
          }
        }
      }
      if (/^[0-9a-f]{64}$/.test(hex)) {
        if (!this.contacts.some((contact) => contact.address === hex)) {
          add({ key: `address:send:${hex}`, kind: "Address", title: "Send to this address", detail: this.short(hex), run: () => this.useRecipient(hex) });
          add({ key: `address:contact:${hex}`, kind: "Address", title: "Save as contact", detail: this.short(hex), run: () => this.openContactEditor(hex) });
        }
        add({ key: `address:copy:${hex}`, kind: "Copy", title: "Copy to clipboard", detail: this.short(hex), run: () => this.copyValue(hex, "Copied") });
      }
      for (const tab of this.tabs()) {
        const title = this.pageTitle(tab);
        if (!number && title.toLowerCase().includes(text)) {
          add({ key: `screen:${tab}`, kind: "Screen", title, detail: this.shortcutLabel(this.shortcutNumber(tab)), run: () => this.setTab(tab) });
        }
      }
      if (this.status.wallet_receive_address && "copy receive address".includes(text)) {
        add({ key: "copy-receive", kind: "Wallet", title: "Copy receive address", detail: this.short(this.status.wallet_receive_address), run: () => this.copyWalletAddress() });
      }
      return results.slice(0, 12);
    },

    async paletteHashResults(hex) {
      const [block, pending] = await Promise.allSettled([
        this.getJson(`/api/blocks/${hex}`),
        this.getJson(`/api/mempool/${hex}`),
      ]);
      const results = [];
      if (block.status === "fulfilled" && block.value?.height != null) {
        const height = Number(block.value.height);
        results.push({ key: `heartbeat:${height}`, kind: "Heartbeat", title: `Heartbeat #${height}`, detail: "Block hash", run: () => this.openHeartbeatHeight(height) });
        if (block.value.ledger) {
          results.push({ key: `ledger-at:${height}`, kind: "Ledger", title: `Ledger block in Heartbeat #${height}`, detail: "Block hash", run: () => this.openHeartbeatHeight(height, "ledger") });
        }
      }
      // Only an answer that carries an amount is a mempool item.
      if (pending.status === "fulfilled" && (pending.value?.total_output != null || pending.value?.budget != null)) {
        const ticket = pending.value.budget != null;
        const item = { ...pending.value, id: hex, kind: ticket ? "Mining ticket" : "Ledger transaction", amount: ticket ? pending.value.budget : pending.value.total_output };
        results.push({ key: `pending:${hex}`, kind: "Pending", title: `${item.kind} · ${this.amountLabel(item.amount)} IUNA`, detail: "In the mempool", run: () => this.openMempoolModal(item) });
      }
      return results;
    },

    async heartbeatSummaryAt(height) {
      const [summary] = await this.getJson(`/api/heartbeats?limit=1&before_height=${Number(height) + 1}`);
      if (Number(summary?.height) !== Number(height)) throw new Error(`Heartbeat #${height} was not found`);
      return summary;
    },

    // A lookup that ends after the user switched screens or opened something else is dropped.
    async navigateWhenFound(lookup, chain) {
      const navigation = ++this.paletteNavigation;
      const tab = this.tab;
      const stale = () => navigation !== this.paletteNavigation || this.tab !== tab;
      let block = await lookup();
      if (stale()) return;
      if (!this.heartbeats.some((item) => item.block_hash === block.block_hash)) {
        block = await this.loadHeartbeatWindow(block);
        if (stale()) return;
      }
      this.openChainBlock(block, chain);
    },

    async openHeartbeatHeight(height, chain = "heartbeat") {
      const loaded = this.heartbeats.find((block) => Number(block.height) === Number(height));
      await this.navigateWhenFound(async () => loaded || this.heartbeatSummaryAt(height), chain);
    },

    // Every Heartbeat records how many Ledger blocks the chain holds up to it, and that count never
    // falls, so the Heartbeat carrying Ledger block N is the first one whose count reaches N.
    async ledgerHeartbeat(number) {
      const target = Number(number);
      const loaded = this.heartbeats.find((block) => block.has_ledger_block && Number(block.ledger_blocks) === target);
      if (loaded) return loaded;
      let low = 0;
      let high = Number(this.status.heartbeat_height || 0);
      while (low < high) {
        const middle = Math.floor((low + high) / 2);
        const summary = await this.heartbeatSummaryAt(middle);
        if (Number(summary.ledger_blocks) >= target) high = middle;
        else low = middle + 1;
      }
      const found = await this.heartbeatSummaryAt(low);
      if (!found.has_ledger_block || Number(found.ledger_blocks) !== target) throw new Error(`Ledger block #${target} was not found`);
      return found;
    },

    async openLedgerHeight(number) {
      const loaded = this.heartbeats.some((block) => block.has_ledger_block && Number(block.ledger_blocks) === Number(number));
      if (!loaded) this.notify(`Looking up Ledger block #${number}…`);
      await this.navigateWhenFound(() => this.ledgerHeartbeat(number), "ledger");
    },

    // The dashboard strip shows the newest 60 Heartbeats, newest first.
    dashboardHeartbeats() {
      return this.heartbeats.slice(0, 60);
    },

    minedByWallet(block) {
      const wallet = String(this.status.wallet_public_key || "").toLowerCase();
      return Boolean(wallet) && String(block?.miner_public_key || "").toLowerCase() === wallet;
    },

    pulseTitle(block) {
      const parts = [`Heartbeat #${block.height}`, this.blockAgeLabel(block, "Genesis")];
      if (block.has_ledger_block) parts.push("Ledger block");
      if (this.minedByWallet(block)) parts.push("mined by you");
      return parts.join(" · ");
    },

    // A bar with a Ledger block opens that block on the Ledger rail; any other bar opens its Heartbeat.
    openPulseBlock(block) {
      this.openChainBlock(block, block.has_ledger_block ? "ledger" : "heartbeat");
    },

    // Opens a Heartbeat summary on the given rail. A block older than the loaded rail stays
    // selected with its details, so only blocks on the rail can be scrolled to.
    async openChainBlock(block, chain) {
      // Only a block outside the loaded rail waits for a request; a loaded one opens at once.
      if (!this.heartbeats.some((item) => item.block_hash === block.block_hash)) block = await this.loadHeartbeatWindow(block);
      const target = chain === "ledger"
        ? this.ledgerBlocks.find((item) => item.block_hash === block.block_hash)
          || { ...block, ledger_height: Number(block.ledger_blocks) }
        : block;
      this.setTab("chain");
      this.setChainView(chain);
      // selectBlock toggles a selected block off, so leave an already selected block alone.
      if (!(this.blockSelected(target) && this.selectedBlockChain === chain)) void this.selectBlock(target, chain);
      // The rail renders after $nextTick and Alpine shows the screen one frame later.
      const nextFrame = globalThis.requestAnimationFrame || ((callback) => setTimeout(callback, 16));
      this.$nextTick(() => nextFrame(() => {
        this.$refs?.[`${chain}Rail`]?.querySelector(".chain-block.selected")
          ?.scrollIntoView({ block: "nearest", inline: "center" });
      }));
    },

    // Puts rows into the loaded Heartbeats, newest first, one per height. Rows that were not
    // loaded next to each other leave a gap the rails show and can fill.
    mergeHeartbeats(rows) {
      const byHeight = new Map(this.heartbeats.map((item) => [Number(item.height), item]));
      for (const row of rows) byHeight.set(Number(row.height), row);
      this.heartbeats = [...byHeight.values()].sort((left, right) => Number(right.height) - Number(left.height));
      this.heartbeatPage.hasMore = Number(this.heartbeats[this.heartbeats.length - 1]?.height) > 0;
      this.syncLedgerBlocks();
    },

    // A block far from the loaded part of the chain gets one page around it, so the rail can show
    // it in place. One request at any chain length; what lies between shows as a gap. Returns the
    // block to open: the node's block at that height, which differs after a reorg.
    async loadHeartbeatWindow(block) {
      const above = Math.floor(this.heartbeatPage.limit / 2);
      try {
        const page = await this.getJson(`/api/heartbeats?limit=${this.heartbeatPage.limit}&before_height=${Number(block.height) + above + 1}`);
        this.mergeHeartbeats(page);
        return page.find((item) => Number(item.height) === Number(block.height)) || block;
      } catch (error) {
        this.notify(error.message, "error");
        return block;
      }
    },

    // How many blocks are missing just before position `index` on a rail; 0 when none are.
    railGap(chain, index) {
      if (index < 1) return 0;
      const blocks = chain === "ledger" ? this.ledgerBlocks : this.heartbeats;
      const key = chain === "ledger" ? "ledger_height" : "height";
      return Math.max(0, Number(blocks[index - 1]?.[key]) - Number(blocks[index]?.[key]) - 1) || 0;
    },

    railGapLabel(chain, index) {
      const count = this.railGap(chain, index);
      const noun = chain === "ledger" ? "Ledger block" : "Heartbeat";
      return `${count.toLocaleString("en-US")} ${noun}${count === 1 ? "" : "s"}`;
    },

    // Shrinks a gap from its newer side. A Heartbeat gap loads the page just below it. A Ledger gap
    // finds the next older Ledger block by bisection and loads the page around it, so each click
    // shows a new Ledger block however sparse they are.
    async loadRailGap(chain, index) {
      const blocks = chain === "ledger" ? this.ledgerBlocks : this.heartbeats;
      const upper = blocks[index - 1];
      if (!upper || this.heartbeatPage.busy) return;
      this.heartbeatPage.busy = true;
      try {
        if (chain === "ledger") {
          await this.loadHeartbeatWindow(await this.ledgerHeartbeat(Number(upper.ledger_height) - 1));
        } else {
          this.mergeHeartbeats(await this.getJson(`/api/heartbeats?limit=${this.heartbeatPage.limit}&before_height=${Number(upper.height)}`));
        }
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.heartbeatPage.busy = false;
      }
    },

    heartbeatAgeLabel() {
      if (this.syncing()) return this.syncingLabel();
      return this.blockAgeLabel(this.heartbeats[0], "No Heartbeats");
    },

    ledgerAgeLabel() {
      if (this.syncing()) return this.syncingLabel();
      return this.blockAgeLabel(this.ledgerBlocks[0], "No Ledger blocks");
    },

    metricsPath() {
      return this.metricsRange === "all" ? "/api/metrics" : `/api/metrics?limit=${this.metricsRange}`;
    },

    cancelMetricsRefresh() {
      this.metricsRequestId += 1;
      iunaMetricsRequests.get(this)?.abort();
      iunaMetricsRequests.delete(this);
      this.metricsBusy = false;
    },

    async refreshMetrics(silent = false) {
      iunaMetricsRequests.get(this)?.abort();
      const controller = typeof AbortController === "function" ? new AbortController() : null;
      if (controller) iunaMetricsRequests.set(this, controller);
      const requestId = ++this.metricsRequestId;
      const path = this.metricsPath();
      this.metricsBusy = true;
      try {
        const rows = await this.getJson(path, controller?.signal);
        if (requestId === this.metricsRequestId && this.tab === "metrics") this.metricsRows = rows;
      } catch (error) {
        if (error?.name !== "AbortError" && !silent && requestId === this.metricsRequestId) {
          this.notify(error.message, "error");
        }
      } finally {
        if (requestId === this.metricsRequestId) {
          iunaMetricsRequests.delete(this);
          this.metricsBusy = false;
        }
      }
    },

    setMetricsRange(range) {
      this.metricsRange = range === 1000 || range === "all" ? range : 100;
      this.chartHover = null;
      try { localStorage.setItem("iunaMetricsRange", String(this.metricsRange)); } catch {}
      this.refreshMetrics();
    },

    metricsLatest() {
      return this.metricsRows[0] || this.heartbeats[0] || {};
    },

    metricsCharts() {
      return [
        { kind: "interval", title: "Heartbeat interval" },
        { kind: "difficulty", title: "Proof-of-work difficulty" },
        { kind: "transactions", title: "Cumulative ledger transactions" },
        { kind: "chainBytes", title: "Compact chain size" },
        { kind: "heartbeatBytes", title: "Heartbeat size" },
        { kind: "tickets", title: "Mining tickets" },
        { kind: "ticketBudget", title: "Ticket budget" },
        { kind: "supply", title: "Total IUNA supply" },
        { kind: "utxos", title: "UTXO set size" },
        { kind: "ledgerBlocks", title: "Cumulative Ledger blocks" },
      ];
    },

    // Alpine evaluates chart expressions repeatedly while updating the DOM. Keep the expensive
    // sort, BigInt difficulty conversion and SVG projection outside its reactive hot path.
    chartCache() {
      const source = this.metricsRows.length ? this.metricsRows : this.heartbeats;
      const first = source[0];
      const last = source[source.length - 1];
      const powLimit = this.status.pow_limit || "";
      const cached = iunaChartCaches.get(this);
      if (cached
          && cached.source === source
          && cached.length === source.length
          && cached.first === first
          && cached.last === last
          && cached.powLimit === powLimit) return cached;
      const cache = {
        source,
        length: source.length,
        first,
        last,
        powLimit,
        blocks: source
          .filter((block) => Number(block.height) > 0)
          .slice()
          .sort((a, b) => a.height - b.height),
        series: {},
        bounds: {},
        polylines: {},
        medianInterval: undefined,
      };
      iunaChartCaches.set(this, cache);
      return cache;
    },

    // Chart series from the selected Metrics range, oldest first.
    chartSeries(kind) {
      const cache = this.chartCache();
      if (cache.series[kind]) return cache.series[kind];
      const blocks = cache.blocks;
      let points = [];
      if (kind === "interval") {
        points = blocks
          .map((block, index) => ({
            x: block.height,
            y: block.interval_seconds == null
              ? (index ? Number(block.timestamp) - Number(blocks[index - 1].timestamp) : null)
              : Number(block.interval_seconds),
          }))
          .filter((point) => point.y != null);
      } else if (kind === "difficulty" && cache.powLimit) {
        const limit = BigInt(`0x${cache.powLimit}`);
        points = blocks.map((block) => {
          const target = BigInt(`0x${block.target}`);
          return { x: block.height, y: Number((limit * 1000n) / (target === 0n ? 1n : target)) / 1000 };
        });
      } else {
        const fields = {
          transactions: "cumulative_ledger_transactions",
          chainBytes: "chain_bytes",
          heartbeatBytes: "heartbeat_bytes",
          tickets: "active_tickets",
          ticketBudget: "ticket_weight",
          supply: "minted",
          utxos: "utxos",
          ledgerBlocks: "ledger_blocks",
        };
        const field = fields[kind];
        if (field) points = blocks.map((block) => ({ x: block.height, y: Number(block[field] || 0) }));
      }
      cache.series[kind] = points;
      return points;
    },

    chartBounds(kind) {
      const cache = this.chartCache();
      if (Object.prototype.hasOwnProperty.call(cache.bounds, kind)) return cache.bounds[kind];
      const points = this.chartSeries(kind);
      if (!points.length) {
        cache.bounds[kind] = null;
        return null;
      }
      let minimum = points[0].y;
      let maximum = points[0].y;
      for (const point of points) {
        minimum = Math.min(minimum, point.y);
        maximum = Math.max(maximum, point.y);
      }
      let low = kind === "interval" ? 0 : minimum;
      // Intervals are drawn up to 10 minutes so one outage does not flatten the chart.
      let high = kind === "interval" ? Math.min(600, Math.max(maximum, 120)) : Math.max(maximum, low);
      if (high === low) { high += 1; low = Math.max(0, low - 1); }
      const ticks = this.niceTicks(low, high, 5);
      low = ticks[0] ?? low;
      high = ticks[ticks.length - 1] ?? high;
      cache.bounds[kind] = { minX: points[0].x, maxX: points[points.length - 1].x, low, high, ticks };
      return cache.bounds[kind];
    },

    niceTicks(minValue, maxValue, maxTicks = 5) {
      const min = Number(minValue);
      const max = Number(maxValue);
      if (!Number.isFinite(min) || !Number.isFinite(max)) return [];
      const step = this.niceTickStep((max - min) / Math.max(1, maxTicks - 1));
      const tickMin = Math.floor(min / step) * step;
      const tickMax = Math.ceil(max / step) * step;
      const precision = Math.max(0, Math.ceil(-Math.log10(step)) + 2);
      const ticks = [];
      for (let tick = tickMin; tick <= tickMax + step / 2; tick += step) {
        ticks.push(Number(tick.toFixed(precision)));
        if (ticks.length > 8) break;
      }
      return ticks;
    },

    niceTickStep(value) {
      if (!Number.isFinite(value) || value <= 0) return 1;
      const exponent = Math.floor(Math.log10(value));
      const fraction = value / (10 ** exponent);
      const niceFraction = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 5 ? 5 : 10;
      return niceFraction * (10 ** exponent);
    },

    chartPoint(kind, point) {
      const bounds = this.chartBounds(kind);
      const spanX = Math.max(1, bounds.maxX - bounds.minX);
      const x = 6 + ((point.x - bounds.minX) / spanX) * 288;
      const value = Math.min(point.y, bounds.high);
      const y = 132 - ((value - bounds.low) / (bounds.high - bounds.low)) * 122;
      return { x, y };
    },

    chartPolyline(kind) {
      const cache = this.chartCache();
      if (Object.prototype.hasOwnProperty.call(cache.polylines, kind)) return cache.polylines[kind];
      if (!this.chartBounds(kind)) return "";
      const points = this.chartDisplaySeries(kind);
      cache.polylines[kind] = points.map((point) => {
        const { x, y } = this.chartPoint(kind, point);
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      }).join(" ");
      return cache.polylines[kind];
    },

    // More points than horizontal SVG pixels add memory and layout work without adding detail.
    // Preserve both extremes per bucket so short spikes remain visible.
    chartDisplaySeries(kind) {
      const points = this.chartSeries(kind);
      if (points.length <= IUNA_CHART_MAX_POINTS) return points;
      const result = [points[0]];
      const bucketCount = Math.floor((IUNA_CHART_MAX_POINTS - 2) / 2);
      const bucketSize = Math.ceil((points.length - 2) / bucketCount);
      for (let start = 1; start < points.length - 1; start += bucketSize) {
        const bucket = points.slice(start, Math.min(points.length - 1, start + bucketSize));
        let low = bucket[0];
        let high = bucket[0];
        for (const point of bucket) {
          if (point.y < low.y) low = point;
          if (point.y > high.y) high = point;
        }
        if (low.x <= high.x) result.push(low, ...(high === low ? [] : [high]));
        else result.push(high, low);
      }
      result.push(points[points.length - 1]);
      return result;
    },

    // Dashed reference line at the 60-second target for the interval chart.
    chartTargetY(kind) {
      const bounds = this.chartBounds(kind);
      if (!bounds || kind !== "interval") return null;
      return 132 - ((60 - bounds.low) / (bounds.high - bounds.low)) * 122;
    },

    chartTicks(kind) {
      const bounds = this.chartBounds(kind);
      if (!bounds) return [];
      return bounds.ticks.slice().reverse();
    },

    chartTickStyle(kind, value) {
      const bounds = this.chartBounds(kind);
      if (!bounds) return "";
      const y = 132 - ((value - bounds.low) / (bounds.high - bounds.low)) * 122;
      return `top:${(y / 148) * 100}%`;
    },

    chartGridPath(kind) {
      const bounds = this.chartBounds(kind);
      if (!bounds) return "";
      return bounds.ticks.map((tick) => {
        const y = 132 - ((tick - bounds.low) / (bounds.high - bounds.low)) * 122;
        return `M6 ${y.toFixed(1)}H294`;
      }).join("");
    },

    chartValueLabel(kind, value) {
      if (value == null) return "-";
      if (kind === "interval") return `${Math.round(value)} s`;
      if (kind === "difficulty") return `${value >= 100 ? Math.round(value) : value.toFixed(2)}×`;
      if (["ticketBudget", "supply"].includes(kind)) return `${this.amountLabel(Math.round(value))} IUNA`;
      if (["chainBytes", "heartbeatBytes"].includes(kind)) return this.bytesLabel(value);
      return Math.round(value).toLocaleString("en-US");
    },

    chartAxisValueLabel(kind, value) {
      if (value == null) return "-";
      if (kind === "interval") return `${this.compactNumber(value)}s`;
      if (kind === "difficulty") return `${this.compactNumber(value)}×`;
      if (["ticketBudget", "supply"].includes(kind)) return this.compactNumber(Number(value) / 100_000_000);
      if (["chainBytes", "heartbeatBytes"].includes(kind)) return this.compactBytesLabel(value);
      return this.compactNumber(value);
    },

    compactBytesLabel(value) {
      const bytes = Math.max(0, Number(value) || 0);
      const units = [[1024 ** 3, "GiB"], [1024 ** 2, "MiB"], [1024, "KiB"]];
      const unit = units.find(([size]) => bytes >= size);
      if (!unit) return `${Math.round(bytes)}B`;
      const scaled = bytes / unit[0];
      const digits = scaled < 10 ? 1 : 0;
      return `${scaled.toFixed(digits).replace(/\.0$/, "")}${unit[1]}`;
    },

    compactNumber(value) {
      const number = Number(value);
      if (!Number.isFinite(number)) return "-";
      const absolute = Math.abs(number);
      const suffixes = [[1e12, "T"], [1e9, "B"], [1e6, "M"], [1e3, "k"]];
      const compact = suffixes.find(([threshold]) => absolute >= threshold);
      if (compact) {
        const scaled = number / compact[0];
        const digits = Math.abs(scaled) < 10 ? 1 : 0;
        return `${scaled.toFixed(digits).replace(/\.0$/, "")}${compact[1]}`;
      }
      if (Number.isInteger(number)) return String(number);
      return number.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
    },

    bytesLabel(value) {
      const bytes = Math.max(0, Number(value) || 0);
      if (bytes < 1024) return `${Math.round(bytes)} B`;
      if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
      if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
      return `${(bytes / 1024 ** 3).toFixed(2)} GiB`;
    },

    chartLatest(kind) {
      const points = this.chartSeries(kind);
      return points.length ? this.chartValueLabel(kind, points[points.length - 1].y) : "-";
    },

    hoverChart(kind, event) {
      const points = this.chartSeries(kind);
      if (!points.length) return;
      const box = event.currentTarget.getBoundingClientRect();
      const ratio = Math.min(1, Math.max(0, (event.clientX - box.left) / box.width));
      const bounds = this.chartBounds(kind);
      const height = bounds.minX + ratio * (bounds.maxX - bounds.minX);
      const point = points.reduce((best, item) => (Math.abs(item.x - height) < Math.abs(best.x - height) ? item : best));
      const position = this.chartPoint(kind, point);
      this.chartHover = { kind, point, left: (position.x / 300) * 100, top: (position.y / 148) * 100 };
    },

    chartTooltip(kind) {
      if (this.chartHover?.kind !== kind) return "";
      return `#${this.chartHover.point.x} · ${this.chartValueLabel(kind, this.chartHover.point.y)}`;
    },

    activityItems() {
      if (!this.activity) return [];
      return [
        ...this.activity.pending.map((item) => ({ ...item, pending: true })),
        ...this.activity.confirmed.map((item) => ({ ...item, pending: false })),
      ];
    },

    applyActivityPage(page, append) {
      if (!page) return;
      if (!this.activity || !append) {
        const preserveOlder = this.activityPageDepth > 1
          && this.activity?.public_key === page.public_key;
        const older = preserveOlder ? (this.activity?.confirmed || []) : [];
        const firstOlderHeight = page.next_before_height;
        const preserved = firstOlderHeight == null
          ? []
          : older.filter((item) => Number(item.heartbeat_height) < Number(firstOlderHeight));
        const confirmed = [...page.confirmed, ...preserved];
        const seen = new Set();
        this.activity = {
          ...page,
          confirmed: confirmed.filter((item) => {
            const key = `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`;
            if (seen.has(key)) return false;
            seen.add(key);
            return true;
          }),
          next_before_height: preserveOlder ? this.activity.next_before_height : page.next_before_height,
        };
        if (!preserveOlder) this.activityPageDepth = 1;
        return;
      }
      const seen = new Set(this.activity.confirmed.map((item) => `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`));
      this.activity.confirmed.push(...page.confirmed.filter((item) => {
        const key = `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      }));
      this.activity.next_before_height = page.next_before_height;
      this.activityPageDepth += 1;
    },

    activityType(item) {
      if (["sent", "received", "self"].includes(item?.kind)) return "transfers";
      if (item?.kind === "ticket") return "tickets";
      if (item?.kind === "heartbeat_reward") return "heartbeatRewards";
      if (item?.kind === "ledger_reward") return "ledgerRewards";
      return "transfers";
    },

    filteredActivityItems() {
      return this.activityItems().filter((item) => this.activityFilters[this.activityType(item)] === true);
    },

    activityFiltersChanged() {
      this.activityVisibleCount = 25;
      this.$nextTick(() => this.loadMoreActivityIfNeeded());
    },

    visibleActivityItems() {
      return this.filteredActivityItems().slice(0, this.activityVisibleCount);
    },

    activityHasMore() {
      return this.activityVisibleCount < this.filteredActivityItems().length
        || this.activity?.next_before_height != null;
    },

    async loadMoreActivity() {
      let grew = false;
      if (this.activityVisibleCount < this.filteredActivityItems().length) {
        this.activityVisibleCount += 25;
        grew = true;
      } else if (this.activity?.next_before_height != null && !this.activityBusy) {
        this.activityBusy = true;
        try {
          const page = await this.getJson(`/api/wallet/activity?before_height=${this.activity.next_before_height}`);
          this.applyActivityPage(page, true);
          this.activityVisibleCount += 25;
          grew = true;
        } catch (error) {
          this.notify(error.message, "error");
        } finally {
          this.activityBusy = false;
        }
      }
      // Re-check only after the list actually grew, and only once the DOM shows it.
      // Alpine drains $nextTick callbacks in one synchronous loop, so scheduling the
      // check unconditionally (while a page is loading or nothing is left) froze the app.
      if (grew) this.$nextTick(() => requestAnimationFrame(() => this.loadMoreActivityIfNeeded()));
    },

    loadMoreActivityIfNeeded() {
      const sentinel = this.$refs?.activitySentinel;
      const scroller = sentinel?.closest(".activity-scroll");
      // A hidden sentinel or tab has no layout box and would always look "in view".
      if (!sentinel || !scroller || !this.activityHasMore()
        || !sentinel.getClientRects().length || !scroller.getClientRects().length) return;
      if (sentinel.getBoundingClientRect().top <= scroller.getBoundingClientRect().bottom + 160) {
        this.loadMoreActivity();
      }
    },

    observeActivitySentinel(element) {
      this.activityObserver?.disconnect();
      if (typeof IntersectionObserver === "undefined") {
        this.activityVisibleCount = Number.MAX_SAFE_INTEGER;
        return;
      }
      this.activityObserver = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) this.loadMoreActivity();
      }, { root: element.closest(".activity-scroll"), rootMargin: "160px" });
      this.activityObserver.observe(element);
    },

    activityLabel(item) {
      return {
        sent: "Sent",
        received: "Received",
        self: "To yourself",
        ticket: "Mining ticket",
        heartbeat_reward: "Heartbeat reward",
        ledger_reward: "Ledger reward",
      }[item.kind] || item.kind;
    },

    activityCode(item) {
      return {
        transfers: "TX",
        tickets: "TI",
        heartbeatRewards: "HR",
        ledgerRewards: "LR",
      }[this.activityType(item)] || "TX";
    },

    activitySign(item) {
      return ["received", "self", "heartbeat_reward", "ledger_reward"].includes(item.kind) ? "+" : "−";
    },

    activityMetaLabel(item) {
      if (item.pending) return "Waiting for confirmation";
      const height = item.heartbeat_height == null ? "" : `Heartbeat #${item.heartbeat_height}`;
      const time = item.timestamp == null ? "" : this.timeLabel(item.timestamp);
      return [height, time].filter(Boolean).join(" · ") || "Confirmed";
    },

    async openUtxos() {
      this.walletView = "utxos";
      this.utxoPage = null;
      this.utxoPageKey = null;
      await this.loadUtxos(false);
    },

    // "More" appends the next page of the same tip. A reload, or a new tip in between, starts
    // again from the first output with as many rows as were shown, so none is listed twice.
    // Reload on a new tip, another wallet, or a mempool change that reserves outputs.
    utxoStatusKey() {
      return `${this.status.wallet_public_key}:${this.status.tip_hash}:${this.status.wallet_spendable_utxos}`;
    },

    async loadUtxos(more) {
      const current = this.utxoPage;
      if (this.utxosLoading) {
        // A load is already running; show the spinner if the pane was just emptied for it.
        if (!current) this.utxosBusy = true;
        return;
      }
      if (more && current?.next_offset == null) return;
      // Recorded only once the load succeeds, so a failed or skipped reload is retried by the
      // next poll.
      const key = this.utxoStatusKey();
      this.utxosLoading = true;
      this.utxosBusy = !current;
      try {
        if (more) {
          const page = await this.getJson(`/api/wallet/utxos?offset=${current.next_offset}`);
          if (page.tip_hash === current.tip_hash && page.public_key === current.public_key) {
            this.utxoPage = { ...page, utxos: [...current.utxos, ...page.utxos] };
            this.utxoPageKey = key;
            return;
          }
        }
        const rows = Math.min(1000, Math.max(200, (current?.utxos.length || 0) + (more ? 200 : 0)));
        this.utxoPage = await this.getJson(`/api/wallet/utxos?limit=${rows}`);
        this.utxoPageKey = key;
      } catch (error) {
        // A background reload fails quietly; the next poll tries again.
        if (more || !current) this.notify(error.message, "error");
      } finally {
        this.utxosLoading = false;
        this.utxosBusy = false;
      }
    },

    async openWalletAddresses() {
      this.walletView = "addresses";
      if (this.walletAddressesBusy) return;
      this.walletAddressesBusy = true;
      this.walletAddresses = null;
      this.walletAddressesError = null;
      try {
        const view = await this.postJson("/api/wallet/addresses", {});
        this.walletAddresses = Array.isArray(view.addresses) ? view.addresses : [];
      } catch (error) {
        this.walletAddressesError = error.message;
        this.notify(error.message, "error");
      } finally {
        this.walletAddressesBusy = false;
      }
    },

    visibleWalletAddresses() {
      const addresses = this.walletAddresses || [];
      if (this.walletAddressFilter === "all") return addresses;
      return this.relevantWalletAddresses();
    },

    relevantWalletAddresses() {
      return (this.walletAddresses || [])
        .filter((item) => item.kind === "mining" || item.used || item.current);
    },

    walletAddressLabel(item) {
      if (item.kind === "mining") return "Mining address";
      const branch = item.kind === "receive" ? "Receive" : "Change";
      return `${branch} #${Number(item.index) + 1}`;
    },

    async copyValue(value, label = "Copied") {
      if (!value) return;
      try {
        await this.copyText(value);
        this.notify(label);
      } catch (error) {
        this.notify(error.message, "error");
      }
    },

    useRecipient(address) {
      this.transfer.recipient = address;
      this.setTab("wallet");
      this.notify("Recipient filled in");
    },

    async changePeer(action, address) {
      if (!this.browserCanManage() || this.peerForm.busy) return;
      this.peerForm.busy = true;
      try {
        this.peerTargets = await this.postJson("/api/peers", { action, address: address.trim() });
        if (action === "add") this.peerForm.address = "";
        this.notify(action === "add" ? `Connecting to ${address.trim()}` : `Removed ${address}`);
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        this.peerForm.busy = false;
      }
    },

    peerTargetState(target) {
      if (target.connected) return "Connected";
      return target.last_attempt ? "Retrying" : "Waiting";
    },

    async changeWalletPassword() {
      const form = this.passwordForm;
      if (form.busy) return;
      if ([...form.next].length < 10) {
        this.notify("Use a new password of at least 10 characters", "error");
        return;
      }
      if (form.next !== form.confirmation) {
        this.notify("The new passwords do not match", "error");
        return;
      }
      form.busy = true;
      try {
        await this.postJson("/api/wallet", { action: "change_password", password: form.current, new_password: form.next });
        Object.assign(form, { current: "", next: "", confirmation: "" });
        this.notify("Wallet password changed");
      } catch (error) {
        this.notify(error.message, "error");
      } finally {
        form.busy = false;
      }
    },

    short(value) {
      const text = String(value || "");
      return text.length > 18 ? `${text.slice(0, 9)}…${text.slice(-7)}` : text || "-";
    },

    timeLabel(timestamp) {
      const value = Number(timestamp);
      if (!Number.isFinite(value)) return "-";
      return new Date(value * 1000).toLocaleString();
    },

    lastUpdatedLabel() {
      return this.lastUpdated ? `Updated ${this.lastUpdated.toLocaleTimeString()}` : "Loading…";
    },

    async copyWalletAddress() {
      const address = this.status.wallet_receive_address;
      if (!address) return;
      try {
        await this.copyText(address);
        this.notify("Wallet address copied");
      } catch (error) {
        this.notify(error.message, "error");
      }
    },

    // navigator.clipboard only exists in secure contexts; plain-HTTP remote access falls back.
    async copyText(text) {
      if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text);
      const field = Object.assign(document.createElement("textarea"), { value: text, readOnly: true });
      document.body.append(field);
      field.select();
      const copied = document.execCommand("copy");
      field.remove();
      if (!copied) throw new Error("Copying is unavailable here; select the address manually");
    },

    notify(message, kind = "success") {
      this.flash = { message, kind };
      clearTimeout(this.flashTimer);
      this.flashTimer = setTimeout(() => { this.flash = null; }, 4500);
    },
  };
};
