import {
  API_BASE, LEGACY_STORAGE_KEY, STORAGE_KEY, api, buildSignedTransfer, decodeVersionedAddress,
  decryptWallet, encodeAddress, encryptWallet, formatIuna, hexToBytes, normalizeWalletStore,
  nextRecoveryScanCounts, parseFeeRate, parseIuna, removeWallet, upsertWallet, walletFromSeed,
  walletId,
} from "./wallet-core.js";
import { generateMnemonic, validateMnemonic } from "./mnemonic.js";
import initQuantumCrypto, {
  build_migration as buildQuantumMigration,
  build_transfer as buildQuantumTransfer,
  derive_external_addresses as deriveExternalAddresses,
  derive_reward_addresses as deriveRewardAddresses,
} from "./crypto/iuna_wallet_crypto.js";

const app = document.querySelector("#app");
const toastElement = document.querySelector("#toast");
const TRANSACTION_PAGE_SIZE = 25;
const ALL_TRANSACTION_FILTERS = { transfer: true, mine: true, burn: true, reward: true };
const state = {
  store: null, wallet: null, walletMeta: null, status: null, address: "", balance: null,
  addresses: [], addressIndex: 0, legacyAddress: "", legacyUtxos: [], hybridUtxos: [],
  derivedAddresses: [], derivedRewardAddresses: [], derivedWalletId: null,
  hybridSpendable: 0, utxos: [], transactions: [], recentTransactions: [], view: "home", timer: null,
  transactionFilters: { ...ALL_TRANSACTION_FILTERS },
  transactionPage: { offset: 0, total: 0, hasMore: true, loading: false, error: "" },
  transactionRequest: 0,
  activityObserver: null,
  selectedTransaction: null,
  transactionReturnView: "home",
};
let quantumCryptoPromise;

async function ensureQuantumCrypto() {
  quantumCryptoPromise ||= initQuantumCrypto();
  await quantumCryptoPromise;
}
const icon = (name) => {
  const paths = {
    send: '<path d="M6 18 18 6M6 6h12v12"/>',
    receive: '<path d="M18 6 6 18M6 6v12h12"/>',
    home: '<rect x="3" y="5" width="18" height="15" rx="3"/><path d="M3 9h18M16 14h2"/>',
    activity: '<path d="M4 6h16M4 12h16M4 18h10"/>',
    settings: '<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3"/><circle cx="15" cy="17" r="3"/>',
    lock: '<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3"/>',
    copy: '<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M15 8V3H3v13h5"/>',
    refresh: '<path d="M20 7v5h-5M4 17v-5h5M6 6a8 8 0 0 1 14 6M18 18a8 8 0 0 1-14-6"/>',
  };
  return '<svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true">' + (paths[name] || paths.activity) + '</svg>';
};

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]);
}

function toast(message, error = false) {
  toastElement.textContent = message;
  toastElement.className = `toast show${error ? " error" : ""}`;
  window.clearTimeout(toastElement.timeout);
  toastElement.timeout = window.setTimeout(() => { toastElement.className = "toast"; }, error ? 7000 : 3000);
}

function readJson(key) {
  try { return JSON.parse(localStorage.getItem(key)); } catch { return null; }
}

function loadStore() {
  const store = normalizeWalletStore(readJson(STORAGE_KEY), readJson(LEGACY_STORAGE_KEY));
  localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  if (localStorage.getItem(LEGACY_STORAGE_KEY)) localStorage.removeItem(LEGACY_STORAGE_KEY);
  return store;
}

function saveStore(store) {
  state.store = store;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
}

function activeWallet() {
  return state.store?.wallets.find((wallet) => wallet.id === state.store.activeId) || null;
}

function defaultWalletName(type) {
  const count = state.store.wallets.filter((wallet) => wallet.type === type).length + 1;
  return type === "readonly" ? `Watch-only ${count}` : `Wallet ${count}`;
}

function logo() {
  return '<div class="brand"><div class="brand-mark" aria-hidden="true"><svg viewBox="0 0 32 32" focusable="false"><circle class="mark-dot" cx="9.4" cy="7.6" r="2.8"></circle><path class="mark-loop" d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13"></path></svg></div><span>iuna wallet</span></div>';
}

function renderWelcome() {
  app.innerHTML = `<section class="onboarding"><div class="hero">${logo()}<h1>Your iuna.<br><em>In your hands.</em></h1><p>A lightweight wallet that connects directly to the iuna network. Manage multiple signing and watch-only wallets on this device.</p></div><div class="onboarding-actions"><button class="button" data-action="create">Create a new wallet</button><button class="button secondary" data-action="import">Use an existing seed</button><button class="button ghost" data-action="watch">Add a watch-only wallet</button><div class="security-note"><span>◇</span><span>Self-custody means only you can recover your seed. iuna cannot retrieve it for you.</span></div></div></section>`;
}

function renderLock() {
  const wallet = activeWallet();
  if (!wallet) { renderWelcome(); return; }
  if (wallet.type === "readonly") { openStoredWallet(wallet); return; }
  app.innerHTML = `<section class="onboarding"><div class="lock-card">${logo()}<p class="eyebrow">${escapeHtml(wallet.name)}</p><h1>Welcome back.</h1><p class="view-copy">Unlock this wallet on this device.</p><form id="unlock-form"><div class="field"><label for="password">Password</label><input id="password" type="password" autocomplete="current-password" autofocus required></div><button class="button" type="submit">Unlock</button></form><button class="button ghost" data-action="wallets" style="width:100%;margin-top:10px">Choose another wallet</button><p class="security-note">Wallet ${escapeHtml(wallet.publicKeyHex?.slice(0, 8))}… is encrypted locally.</p></div></section>`;
}

function renderWalletPicker() {
  const items = state.store.wallets.map((wallet) => `<button class="wallet-row ${wallet.id === state.store.activeId ? "active" : ""}" data-wallet-id="${escapeHtml(wallet.id)}"><span><strong>${escapeHtml(wallet.name)}</strong><small>${wallet.type === "readonly" ? "Watch-only" : "Signing wallet"} · ${escapeHtml(wallet.publicKeyHex.slice(0, 10))}…</small></span><span>›</span></button>`).join("");
  app.innerHTML = `<section class="onboarding"><div>${logo()}<p class="eyebrow picker-title">Wallets</p><h1 class="view-title">Choose a wallet</h1><div class="wallet-list">${items}</div></div><div class="onboarding-actions"><button class="button" data-action="create">Create a new wallet</button><button class="button secondary" data-action="import">Import a seed</button><button class="button ghost" data-action="watch">Add watch-only</button></div></section>`;
}

function renderWatchOnly() {
  app.innerHTML = `<section class="onboarding"><div><button class="back" data-action="back-to-wallets">← Back</button><p class="eyebrow">Watch-only</p><h1 class="view-title">Follow an address.</h1><p class="view-copy">View its balance and activity without storing a seed or private key. This wallet can never sign transactions.</p><form id="watch-form"><div class="field"><label for="wallet-name">Wallet name</label><input id="wallet-name" maxlength="40" placeholder="Savings" required></div><div class="field"><label for="watch-address">Mainnet iuna address</label><input id="watch-address" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="iuna1q…" required></div><button class="button" type="submit" style="width:100%">Add watch-only wallet</button></form></div><div class="security-note" style="margin-top:auto"><span>◇</span><span>Only the public address is saved on this device.</span></div></section>`;
  document.querySelector("#wallet-name").value = defaultWalletName("readonly");
}

function renderImport() {
  app.innerHTML = `<section class="onboarding"><div><button class="back" data-action="back-to-wallets">← Back</button><p class="eyebrow">Recover wallet</p><h1 class="view-title">Existing seed</h1><p class="view-copy">Enter the 24 words in the same order. They are processed locally only.</p><form id="import-form"><div class="field"><label for="import-name">Wallet name</label><input id="import-name" maxlength="40" required></div><div class="field"><label for="seed">Seed phrase</label><textarea id="seed" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="word 1  word 2  word 3 …" required></textarea></div><div class="field"><label for="new-password">New wallet password</label><input id="new-password" type="password" minlength="10" autocomplete="new-password" placeholder="At least 10 characters" required></div><button class="button" type="submit" style="width:100%">Recover wallet</button></form></div><div class="security-note" style="margin-top:auto"><span>◇</span><span>Your seed never leaves this browser.</span></div></section>`;
  document.querySelector("#import-name").value = defaultWalletName("signing");
}

async function renderNewSeed() {
  app.innerHTML = `<section class="center-card"><div class="spinner"></div><p>Creating a secure seed…</p></section>`;
  const seed = await generateMnemonic();
  const words = seed.split(" ").map((word) => `<div class="seed-word">${word}</div>`).join("");
  app.innerHTML = `<section class="onboarding"><div><button class="back" data-action="back-to-wallets">← Back</button><p class="eyebrow">Step 1 of 2</p><h1 class="view-title">Save your seed.</h1><p class="view-copy">Write these 24 words on paper in this exact order.</p><div class="seed-grid">${words}</div><div class="warning">Do not take a screenshot. Anyone with these words can access your wallet.</div><button class="button secondary" data-action="copy-seed" style="width:100%">Copy seed</button><button class="button" data-action="seed-saved" style="width:100%;margin-top:10px">I have saved the words</button></div></section>`;
  app.dataset.pendingSeed = seed;
}

function renderPasswordSetup() {
  const seed = app.dataset.pendingSeed;
  app.innerHTML = `<section class="onboarding" data-seed="${escapeHtml(seed)}"><div><button class="back" data-action="create">← Back</button><p class="eyebrow">Step 2 of 2</p><h1 class="view-title">Secure this device.</h1><p class="view-copy">This password encrypts your seed before it is stored in the browser.</p><form id="create-form"><div class="field"><label for="create-name">Wallet name</label><input id="create-name" maxlength="40" required></div><div class="field"><label for="create-password">Password</label><input id="create-password" type="password" minlength="10" autocomplete="new-password" required></div><div class="field"><label for="confirm-password">Repeat password</label><input id="confirm-password" type="password" minlength="10" autocomplete="new-password" required></div><button class="button" type="submit" style="width:100%">Open wallet</button></form></div></section>`;
  document.querySelector("#create-name").value = defaultWalletName("signing");
  app.dataset.pendingSeed = seed;
}

async function saveAndOpen(seed, password, name) {
  const walletName = name.trim();
  if (!walletName) throw new Error("Enter a wallet name");
  const wallet = await walletFromSeed(seed);
  const record = await encryptWallet(seed, password, wallet.publicKeyHex);
  const meta = { id: walletId(wallet.publicKeyHex), name: walletName, type: "signing", publicKeyHex: wallet.publicKeyHex, record };
  saveStore(upsertWallet(state.store, meta));
  state.wallet = wallet;
  state.walletMeta = meta;
  await openWallet();
}

async function openStoredWallet(meta) {
  state.walletMeta = meta;
  state.wallet = meta.type === "readonly"
    ? { type: "readonly", publicKeyHex: meta.publicKeyHex, address: meta.address || "", publicKey: Uint8Array.from(meta.publicKeyHex.match(/.{2}/g), (pair) => Number.parseInt(pair, 16)) }
    : state.wallet;
  await openWallet();
}

async function fetchWalletData() {
  const previousAddress = state.address;
  state.status = await api("/status");
  state.legacyAddress = state.walletMeta?.type === "readonly" && state.wallet.address
    ? state.wallet.address
    : encodeAddress(state.wallet.publicKey, state.status.chain_id);
  let snapshot;
  if (state.walletMeta?.type === "readonly") {
    const address = state.wallet.address || state.legacyAddress;
    snapshot = await fetchSnapshot([address]);
    state.addresses = [address];
    state.addressIndex = 0;
    state.address = address;
  } else if (state.status.transaction_v2_active) {
    if (Number(state.status.api_version || 0) < 3) throw new Error("The public iuna endpoint must be upgraded for complete rotating-wallet recovery");
    await ensureQuantumCrypto();
    const gapLimit = Math.max(1, Number(state.status.hybrid_address_gap_limit || 20));
    let externalCount = gapLimit + 1;
    let rewardCount = gapLimit;
    let derived;
    let rewardDerived;
    const derivationScope = `${state.walletMeta.id}:${state.status.chain_id}`;
    if (state.derivedWalletId !== derivationScope) {
      state.derivedAddresses = [];
      state.derivedRewardAddresses = [];
      state.derivedWalletId = derivationScope;
    } else {
      externalCount = Math.max(externalCount, state.derivedAddresses.length);
      rewardCount = Math.max(rewardCount, state.derivedRewardAddresses.length);
    }
    for (;;) {
      if (state.derivedAddresses.length < externalCount) {
        state.derivedAddresses = JSON.parse(deriveExternalAddresses(state.wallet.seedPhrase, externalCount, state.status.chain_id));
      }
      if (state.derivedRewardAddresses.length < rewardCount) {
        state.derivedRewardAddresses = JSON.parse(deriveRewardAddresses(state.wallet.seedPhrase, rewardCount, state.status.chain_id));
      }
      derived = state.derivedAddresses.slice(0, externalCount);
      rewardDerived = state.derivedRewardAddresses.slice(0, rewardCount);
      snapshot = await fetchSnapshot([
        state.legacyAddress,
        ...derived.map((item) => item.address),
        ...rewardDerived.map((item) => item.address),
      ], state.legacyAddress);
      const recoveryScan = nextRecoveryScanCounts({
        externalAddresses: derived,
        rewardAddresses: rewardDerived,
        recoveryAddresses: snapshot.recovery_addresses,
        gapLimit,
      });
      if (recoveryScan.unresolved.length) {
        if (recoveryScan.externalCount === externalCount && recoveryScan.rewardCount === rewardCount) {
          throw new Error("Wallet address recovery exceeded its recovery limit");
        }
        externalCount = recoveryScan.externalCount;
        rewardCount = recoveryScan.rewardCount;
        continue;
      }
      const used = new Set(snapshot.addresses.filter((item) => item.used).map((item) => item.address));
      const highestExternal = derived.reduce((highest, item) => used.has(item.address) ? Math.max(highest, item.index) : highest, -1);
      const highestReward = rewardDerived.reduce((highest, item) => used.has(item.address) ? Math.max(highest, item.index) : highest, -1);
      const nextExternalCount = highestExternal < externalCount - gapLimit || externalCount >= 10_000
        ? externalCount
        : Math.min(10_000, highestExternal + gapLimit + 1);
      const nextRewardCount = highestReward < rewardCount - gapLimit || rewardCount >= 10_000
        ? rewardCount
        : Math.min(10_000, highestReward + gapLimit + 1);
      if (nextExternalCount === externalCount && nextRewardCount === rewardCount) break;
      externalCount = nextExternalCount;
      rewardCount = nextRewardCount;
    }
    const used = new Set(snapshot.addresses.filter((item) => item.used).map((item) => item.address));
    const highestUsed = derived.reduce((highest, item) => used.has(item.address) ? Math.max(highest, item.index) : highest, -1);
    state.addressIndex = highestUsed + 1;
    const current = derived.find((item) => item.index === state.addressIndex);
    if (!current) throw new Error("Wallet address discovery exceeded its recovery limit");
    state.addresses = [
      state.legacyAddress,
      ...derived.map((item) => item.address),
      ...rewardDerived.map((item) => item.address),
    ];
    state.address = current.address;
    const descriptorByAddress = new Map([
      ...derived.map((item) => [item.address, { addressIndex: item.index, addressBranch: "external" }]),
      ...rewardDerived.map((item) => [item.address, { addressIndex: item.index, addressBranch: "reward" }]),
    ]);
    state.legacyUtxos = (snapshot.utxos || []).filter((utxo) => utxo.address === state.legacyAddress);
    state.hybridUtxos = (snapshot.utxos || [])
      .filter((utxo) => descriptorByAddress.has(utxo.address))
      .map((utxo) => ({ ...utxo, ...descriptorByAddress.get(utxo.address) }));
    state.hybridSpendable = snapshot.addresses.filter((item) => item.version === 1).reduce((sum, item) => sum + Number(item.spendable || 0), 0);
  } else {
    snapshot = await fetchSnapshot([state.legacyAddress]);
    state.addresses = [state.legacyAddress];
    state.addressIndex = 0;
    state.address = state.legacyAddress;
    state.legacyUtxos = snapshot.utxos || [];
    state.hybridUtxos = [];
    state.hybridSpendable = 0;
  }
  const address = state.address;
  if (previousAddress && previousAddress !== address) {
    state.transactions = [];
    state.recentTransactions = [];
    Object.assign(state.transactionPage, { offset: 0, total: 0, hasMore: true, loading: false, error: "" });
  }
  state.address = address;
  const previousTransactionCount = state.transactions.length;
  const transactionLimit = Math.min(100, Math.max(TRANSACTION_PAGE_SIZE, previousTransactionCount));
  const refreshTransactions = !state.transactionPage.loading;
  const transactionRequest = refreshTransactions ? ++state.transactionRequest : null;
  const [transactions, recentTransactions] = await Promise.all([
    refreshTransactions ? fetchTransactions(0, transactionLimit) : Promise.resolve(null),
    fetchTransactions(0, 5, ALL_TRANSACTION_FILTERS),
  ]);
  state.balance = snapshot;
  state.utxos = snapshot.utxos || [];
  state.recentTransactions = normalizeTransactionItems(recentTransactions?.items);
  if (transactions && transactionRequest === state.transactionRequest) {
    transactions.items = normalizeTransactionItems(transactions.items);
    applyTransactionPage(transactions, true, previousTransactionCount);
  }
}

function fetchSnapshot(addresses, recoveryAddress = null) {
  return api("/wallets/snapshot", {
    method: "POST",
    body: JSON.stringify({ addresses, ...(recoveryAddress ? { recovery_address: recoveryAddress } : {}) }),
  });
}

function fetchTransactions(offset, limit, filters = state.transactionFilters) {
  return api("/wallets/transactions", {
    method: "POST",
    body: JSON.stringify({ addresses: state.addresses, ...filters, offset, limit }),
  });
}

function normalizeTransactionItems(items) {
  return Array.isArray(items) ? items.map((row) => ({
    kind: row.kind === "migration" ? "transfer" : row.kind,
    status: row.status,
    block_height: row.blockHeight,
    timestamp_ms: row.timestampMs,
    direction: row.direction,
    transaction: {
      kind: row.kind,
      inputs: row.inputs || [],
      outputs: row.outputs || [],
      change: row.change || [],
      amount: row.amount || 0,
      fee: row.fee || 0,
      signature: row.signature,
    },
  })) : [];
}

function transactionKey(item) {
  return `${item.kind || ""}:${item.transaction?.signature || ""}`;
}

function networkAddress(publicKey) {
  if (!publicKey) return "Unknown";
  if (/^(t?iuna)1/i.test(publicKey)) return publicKey;
  try {
    return encodeAddress(hexToBytes(publicKey), state.status?.chain_id);
  } catch {
    return String(publicKey);
  }
}

function applyTransactionPage(payload, replace, preserveCount = 0) {
  const items = Array.isArray(payload?.items) ? payload.items : [];
  state.transactionPage.error = "";
  if (replace) {
    if (preserveCount > items.length && payload?.has_more === true) {
      const fresh = new Set(items.map(transactionKey));
      const retained = state.transactions.filter((item) => !fresh.has(transactionKey(item)));
      state.transactions = items.concat(retained).slice(0, preserveCount);
    } else {
      state.transactions = items;
    }
  } else {
    const known = new Set(state.transactions.map(transactionKey));
    state.transactions = state.transactions.concat(items.filter((item) => {
      const key = transactionKey(item);
      if (known.has(key)) return false;
      known.add(key);
      return true;
    }));
  }
  state.transactionPage.offset = preserveCount > items.length
    ? state.transactions.length
    : Number(payload?.next_offset ?? (replace ? items.length : state.transactionPage.offset + items.length));
  state.transactionPage.total = Number(payload?.total ?? state.transactions.length);
  state.transactionPage.hasMore = state.transactions.length < state.transactionPage.total;
}

async function loadTransactions({ replace = false } = {}) {
  if (!state.wallet || (state.transactionPage.loading && !replace)) return;
  const request = ++state.transactionRequest;
  if (replace) {
    state.transactions = [];
    Object.assign(state.transactionPage, { offset: 0, total: 0, hasMore: true });
  } else if (!state.transactionPage.hasMore) {
    return;
  }
  state.transactionPage.loading = true;
  state.transactionPage.error = "";
  if (state.view === "activity") renderApp();
  try {
    const offset = replace ? 0 : state.transactionPage.offset;
    const payload = await fetchTransactions(offset, TRANSACTION_PAGE_SIZE);
    if (request !== state.transactionRequest) return;
    payload.items = normalizeTransactionItems(payload.items);
    applyTransactionPage(payload, replace);
  } catch (error) {
    if (request === state.transactionRequest) {
      state.transactionPage.error = error.message;
      toast(error.message, true);
    }
  } finally {
    if (request === state.transactionRequest) {
      state.transactionPage.loading = false;
      if (state.view === "activity") renderApp();
    }
  }
}

async function openWallet() {
  state.transactionRequest += 1;
  state.transactionPage.loading = false;
  state.activityObserver?.disconnect();
  state.activityObserver = null;
  app.innerHTML = `<section class="center-card"><div class="spinner"></div><p>Connecting to iuna…</p></section>`;
  try {
    await fetchWalletData();
    renderApp();
    window.clearInterval(state.timer);
    state.timer = window.setInterval(refreshSilently, 20_000);
  } catch (error) {
    renderOffline(error.message);
  }
}

function renderOffline(message) {
  app.innerHTML = `<section class="onboarding"><div class="lock-card">${logo()}<p class="eyebrow">Connection failed</p><h1 class="view-title">Endpoint unavailable.</h1><p class="view-copy">${escapeHtml(message)}</p><button class="button" data-action="retry" style="width:100%">Try again</button><button class="button ghost" data-action="lock" style="width:100%;margin-top:10px">Lock wallet</button><p class="security-note">Endpoint: ${escapeHtml(API_BASE)}</p></div></section>`;
}

async function refreshSilently() {
  if (!state.wallet) return;
  try { await fetchWalletData(); if (["home", "activity"].includes(state.view)) renderApp(); } catch { /* keep the last known state */ }
}

function topbar() {
  const online = state.status?.ready;
  return `<header class="topbar"><button class="wallet-button" data-action="wallets" aria-label="Switch wallet">${logo()}<span><strong>${escapeHtml(state.walletMeta?.name)}</strong><small>${state.walletMeta?.type === "readonly" ? "Watch-only" : "Signing"}</small></span></button><div style="display:flex;align-items:center;gap:10px"><div class="network"><span class="dot ${online ? "live" : ""}"></span>${online ? "Mainnet" : "Syncing"}</div><button class="icon-button" data-action="lock" aria-label="Lock">${icon("lock")}</button></div></header>`;
}

function nav() {
  return `<nav class="bottom-nav">${[["home","Overview"],["send","Send"],["receive","Receive"],["settings","Settings"]].map(([view,label]) => `<button class="nav-item ${state.view === view ? "active" : ""}" data-view="${view}"><span>${icon(view)}</span>${label}</button>`).join("")}</nav>`;
}

function transactionInfo(item) {
  const tx = item.transaction || {};
  const owns = (address) => state.addresses.includes(address) || address === state.wallet.publicKeyHex;
  if (tx.kind === "migration") return { title: "Migrated", incoming: true, amount: tx.amount || 0, symbol: "◇" };
  if (item.kind === "reward") {
    const amount = tx.outputs?.filter((output) => owns(output.address)).reduce((sum, output) => sum + Number(output.amount || 0), 0) || 0;
    return { title: "Block reward", incoming: true, amount, symbol: "★" };
  }
  if (tx.kind === "mine") return { title: "Mining reward", incoming: true, amount: 1_000_000, symbol: "✦" };
  if (tx.kind === "burn") return { title: "Burn", incoming: false, amount: tx.amount || 0, symbol: "×" };
  const sent = tx.inputs?.some((input) => owns(input.owner));
  const relevant = tx.outputs?.filter((output) => sent ? !owns(output.address) : owns(output.address)) || [];
  return { title: sent ? "Sent" : "Received", incoming: !sent, amount: relevant.reduce((sum, output) => sum + Number(output.amount || 0), 0), symbol: sent ? "↗" : "↙" };
}

function formatTransactionDate(item) {
  if (!item.timestamp_ms) return "Pending";
  const timestamp = new Date(Number(item.timestamp_ms));
  if (Number.isNaN(timestamp.getTime())) return "Unknown time";
  return timestamp.toLocaleString("en-GB", {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  }).replace(",", "");
}

function activityList(limit, filtered = false, source = state.transactions) {
  const items = typeof limit === "number" ? source.slice(0, limit) : source;
  if (!items.length && filtered && state.transactionPage.loading) return '';
  if (!items.length) return filtered
    ? '<div class="empty">No transactions match these filters.</div>'
    : '<div class="empty">No transactions yet.<br>Your new wallet is ready to use.</div>';
  return `<div class="activity-list">${items.map((item) => {
    const info = transactionInfo(item);
    const date = formatTransactionDate(item);
    return `<button class="activity" data-transaction-key="${escapeHtml(transactionKey(item))}" type="button" aria-label="View ${escapeHtml(info.title.toLowerCase())} transaction details"><span class="activity-icon">${info.symbol}</span><span><span class="activity-title">${info.title}</span><span class="activity-meta">${escapeHtml(date)} · ${escapeHtml(item.status)}</span></span><span class="activity-amount ${info.incoming ? "in" : ""}">${info.incoming ? "+" : "−"}${formatIuna(info.amount, 4)} IUNA</span><span class="activity-chevron" aria-hidden="true">›</span></button>`;
  }).join("")}</div>`;
}

function addressDetail(label, address, amount = null) {
  const formattedAmount = amount === null ? "" : `<small>${formatIuna(amount, 6)} IUNA</small>`;
  return `<div class="address-detail"><span>${escapeHtml(label)}</span><div><code>${escapeHtml(address)}</code>${formattedAmount}</div></div>`;
}

function transactionFlow(item) {
  const tx = item.transaction || {};
  if (item.kind === "reward") {
    return {
      from: [{ label: "From", address: "Network reward", amount: null }],
      to: (tx.outputs || []).map((output) => ({ label: "To", address: networkAddress(output.address), amount: output.amount })),
    };
  }
  if (tx.kind === "mine") {
    return {
      from: [{ label: "From", address: "Mining protocol", amount: null }],
      to: [{ label: "To", address: networkAddress(tx.recipient), amount: 1_000_000 }],
    };
  }
  const owners = [...new Set((tx.inputs || []).map((input) => input.owner).filter(Boolean))];
  const outputs = tx.kind === "burn" ? (tx.change || []) : (tx.outputs || []);
  const from = owners.map((owner) => ({ label: "From", address: networkAddress(owner), amount: null }));
  const to = outputs.map((output) => ({
    label: owners.includes(output.address) ? "Change to" : "To",
    address: networkAddress(output.address),
    amount: output.amount,
  }));
  if (tx.kind === "burn") to.unshift({ label: "To", address: "Burned permanently", amount: tx.amount || 0 });
  return {
    from: from.length ? from : [{ label: "From", address: "Unknown", amount: null }],
    to: to.length ? to : [{ label: "To", address: "Unknown", amount: null }],
  };
}

function renderTransaction() {
  const item = state.selectedTransaction;
  if (!item) {
    state.view = state.transactionReturnView;
    return state.view === "activity" ? renderActivity() : renderHome();
  }
  const tx = item.transaction || {};
  const info = transactionInfo(item);
  const flow = transactionFlow(item);
  const transactionId = tx.signature || "Unknown";
  const fee = Number(tx.fee || 0);
  const addresses = [...flow.from, ...flow.to].map((entry) => addressDetail(entry.label, entry.address, entry.amount)).join("");
  return `${topbar()}<button class="back" data-action="close-transaction">← Back</button><p class="eyebrow">Transaction details</p><h1 class="view-title">${escapeHtml(info.title)}</h1><p class="transaction-total ${info.incoming ? "in" : ""}">${info.incoming ? "+" : "−"}${formatIuna(info.amount, 6)} IUNA</p><div class="panel transaction-detail">${addresses}<div class="detail-line"><span>Status</span><strong class="status-value">${escapeHtml(item.status || "Unknown")}</strong></div><div class="detail-line"><span>Date</span><strong>${escapeHtml(formatTransactionDate(item))}</strong></div>${item.block_height === null || item.block_height === undefined ? "" : `<div class="detail-line"><span>Block</span><strong>${escapeHtml(item.block_height)}</strong></div>`}${fee ? `<div class="detail-line"><span>Network fee</span><strong>${formatIuna(fee, 6)} IUNA</strong></div>` : ""}<div class="transaction-id"><span>Transaction ID</span><code>${escapeHtml(transactionId)}</code><button class="icon-button" data-action="copy-transaction-id" aria-label="Copy transaction ID">${icon("copy")}</button></div></div>`;
}

function renderHome() {
  const readonly = state.walletMeta?.type === "readonly";
  const legacySpendable = state.legacyUtxos.reduce((sum, utxo) => sum + Number(utxo.output.amount || 0), 0);
  const migration = !readonly && state.status?.transaction_v2_active && legacySpendable > 0
    ? `<div class="panel migration-card"><p class="eyebrow">Quantum-resistant wallet</p><h2>Migrate ${formatIuna(legacySpendable, 6)} IUNA</h2><p>Move the remaining legacy outputs into your rotating hybrid wallet.</p><button class="button" data-action="migrate">Migrate now</button></div>`
    : "";
  return `${topbar()}<section>${readonly ? '<div class="mode-badge">Watch-only · signing disabled</div>' : ""}<p class="eyebrow">Available balance</p><h1 class="balance">${formatIuna(state.balance?.spendable, 6)} <span>IUNA</span></h1><p class="subbalance">${formatIuna(state.balance?.confirmed, 6)} confirmed · block ${escapeHtml(state.balance?.height)}</p><div class="actions"><button class="button" data-view="send" ${readonly ? "disabled" : ""}>${icon("send")} Send</button><button class="button secondary" data-view="receive">${icon("receive")} Receive</button></div>${migration}<div class="section-head"><h2>Recent activity</h2><button data-view="activity">View all</button></div><div class="panel">${activityList(5, false, state.recentTransactions)}</div></section>`;
}

function renderSend() {
  if (state.walletMeta?.type === "readonly") return `${topbar()}<p class="eyebrow">Watch-only</p><h1 class="view-title">Sending is disabled.</h1><p class="view-copy">This wallet contains no seed or private key, so it cannot sign transactions.</p><button class="button secondary" data-view="home" style="width:100%">Back to overview</button>`;
  const defaultFeeRate = state.status.default_fee_per_byte ?? 1;
  const available = state.status?.transaction_v2_active ? state.hybridSpendable : state.balance?.spendable;
  return `${topbar()}<p class="eyebrow">Transaction</p><h1 class="view-title">Send IUNA</h1><p class="view-copy">The transaction is signed on this device with your hybrid quantum-resistant key.</p><form id="send-form" class="panel send-card"><div class="field"><label for="recipient">Recipient</label><input id="recipient" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="iuna1p…" required></div><div class="field"><label for="amount">Amount</label><div class="amount-wrap"><input id="amount" inputmode="decimal" placeholder="0.00" required><span>IUNA</span></div></div><div class="field"><label for="fee-rate">Fee rate (µIUNA per byte)</label><input id="fee-rate" name="fee-rate" type="number" inputmode="numeric" min="1" step="1" value="${escapeHtml(defaultFeeRate)}" required></div><div class="fee-line"><span>Available</span><strong>${formatIuna(available)} IUNA</strong></div><button class="button" type="submit">Review transaction</button></form>`;
}

function renderReceive() {
  const readonly = state.walletMeta?.type === "readonly";
  const explanation = readonly
    ? "This watch-only entry follows this address only; it cannot derive future rotating addresses."
    : "This address rotates automatically after funds arrive. Previous addresses remain part of your wallet.";
  return `${topbar()}<p class="eyebrow">Your address</p><h1 class="view-title">Receive IUNA</h1><p class="view-copy">Share this current mainnet address with the sender.</p><div class="panel"><div class="receive-emblem">${icon("receive")}</div><p class="eyebrow">${readonly ? "Watched address" : "Current receiving address"}</p><div class="address-box"><code>${escapeHtml(state.address)}</code><button class="icon-button" data-action="copy-address" aria-label="Copy address">${icon("copy")}</button></div><p class="security-note">${explanation}</p></div>`;
}

function renderActivity() {
  const filters = [["transfer", "Tx"], ["mine", "Mine"], ["burn", "Burn"], ["reward", "Reward"]]
    .map(([kind, label]) => `<button class="transaction-filter ${state.transactionFilters[kind] ? "active" : ""}" data-transaction-filter="${kind}" aria-pressed="${state.transactionFilters[kind]}">${label}</button>`)
    .join("");
  const loader = state.transactionPage.loading ? '<div class="activity-loader"><span class="spinner"></span><span>Loading transactions…</span></div>' : '';
  const retry = state.transactionPage.error ? '<button class="activity-retry" data-action="retry-transactions">Loading failed · try again</button>' : '';
  const sentinel = state.transactionPage.hasMore && !state.transactionPage.error ? '<div id="activity-sentinel" class="activity-sentinel" aria-hidden="true"></div>' : '';
  return `${topbar()}<p class="eyebrow">Wallet</p><h1 class="view-title">Activity</h1><p class="view-copy">Confirmed and pending transactions.</p><div class="transaction-filters" aria-label="Transaction filters">${filters}</div><div class="panel">${activityList(undefined, true)}${loader}${retry}${sentinel}</div>`;
}

function renderSettings() {
  const security = state.walletMeta?.type === "readonly" ? "Watch-only · no private key stored" : "AES-256-GCM · PBKDF2-SHA256 · 310,000 iterations";
  return `${topbar()}<p class="eyebrow">Wallet</p><h1 class="view-title">Settings</h1><div class="panel"><div class="setting"><h3>Name</h3><p>${escapeHtml(state.walletMeta?.name)}</p></div><div class="setting"><h3>Network</h3><p>${escapeHtml(state.status.network_id)} · block ${escapeHtml(state.status.height)}</p></div><div class="setting"><h3>Public endpoint</h3><p>${escapeHtml(API_BASE)}</p></div><div class="setting"><h3>Local security</h3><p>${security}</p></div><div class="setting"><h3>Wallet address</h3><p style="word-break:break-all">${escapeHtml(state.address)}</p></div></div><button class="button secondary" data-action="wallets" style="width:100%;margin-top:12px">Switch or add wallet</button><button class="button ghost" data-action="lock" style="width:100%;margin-top:10px">Lock wallet</button><button class="button danger" data-action="forget" style="width:100%;margin-top:10px">Remove this wallet</button><p class="security-note">iuna is experimental software. Only use funds you can afford to lose.</p>`;
}

function renderApp() {
  const renderers = { home: renderHome, send: renderSend, receive: renderReceive, activity: renderActivity, transaction: renderTransaction, settings: renderSettings };
  state.activityObserver?.disconnect();
  state.activityObserver = null;
  app.innerHTML = `${(renderers[state.view] || renderHome)()}${nav()}`;
  if (state.view === "activity") {
    const sentinel = document.querySelector("#activity-sentinel");
    if (sentinel) {
      state.activityObserver = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) loadTransactions();
      }, { rootMargin: "240px 0px" });
      state.activityObserver.observe(sentinel);
    }
  }
}

function renderConfirmation(transaction, fee, recipientAddress, amount) {
  app.innerHTML = `${topbar()}<button class="back" data-view="send">← Edit</button><p class="eyebrow">Review</p><h1 class="view-title">Does everything look right?</h1><div class="panel"><div class="detail-line"><span>You send</span><strong>${formatIuna(amount)} IUNA</strong></div><div class="detail-line"><span>To</span><strong>${escapeHtml(recipientAddress.slice(0, 12))}…${escapeHtml(recipientAddress.slice(-8))}</strong></div><div class="detail-line"><span>Network fee</span><strong>${formatIuna(fee)} IUNA</strong></div><div class="detail-line"><span>Total</span><strong>${formatIuna(amount + fee)} IUNA</strong></div></div><button class="button" id="confirm-send" style="width:100%;margin-top:14px">Sign & send</button><p class="security-note">This action cannot be reversed after submission.</p>${nav()}`;
  document.querySelector("#confirm-send").addEventListener("click", async (event) => {
    const button = event.currentTarget;
    button.disabled = true; button.innerHTML = '<span class="spinner"></span> Sending…';
    try {
      const isV2 = typeof transaction.envelope === "string";
      const result = await api(isV2 ? "/transactions-v2" : "/transactions", {
        method: "POST",
        body: JSON.stringify(isV2 ? { envelope: transaction.envelope } : transaction),
        timeoutMs: 20_000,
      });
      toast(result.status === "accepted" ? "Transaction sent" : "Transaction was already known");
      state.view = "home";
      await fetchWalletData();
      renderApp();
    } catch (error) { toast(error.message, true); button.disabled = false; button.textContent = "Try again"; }
  });
}

app.addEventListener("click", async (event) => {
  const button = event.target.closest("button");
  if (!button) return;
  const action = button.dataset.action;
  if (button.dataset.transactionKey) {
    const key = button.dataset.transactionKey;
    const item = [...state.recentTransactions, ...state.transactions].find((transaction) => transactionKey(transaction) === key);
    if (item) {
      state.transactionReturnView = state.view === "activity" ? "activity" : "home";
      state.selectedTransaction = item;
      state.view = "transaction";
      renderApp();
    }
    return;
  }
  if (action === "close-transaction") {
    state.selectedTransaction = null;
    state.view = state.transactionReturnView;
    renderApp();
    return;
  }
  if (action === "copy-transaction-id") {
    await navigator.clipboard.writeText(state.selectedTransaction?.transaction?.signature || "");
    toast("Transaction ID copied");
    return;
  }
  if (button.dataset.transactionFilter) {
    const filter = button.dataset.transactionFilter;
    state.transactionFilters[filter] = !state.transactionFilters[filter];
    await loadTransactions({ replace: true });
    return;
  }
  if (action === "retry-transactions") {
    await loadTransactions({ replace: state.transactions.length === 0 });
    return;
  }
  if (button.dataset.view) {
    if (button.dataset.view === "send" && state.walletMeta?.type === "readonly") { toast("Watch-only wallets cannot send", true); return; }
    state.view = button.dataset.view; renderApp(); return;
  }
  if (button.dataset.walletId) {
    saveStore({ ...state.store, activeId: button.dataset.walletId });
    state.wallet = null; state.walletMeta = null; state.view = "home";
    renderLock();
    return;
  }
  if (action === "welcome") renderWelcome();
  if (action === "back-to-wallets") state.store.wallets.length ? renderWalletPicker() : renderWelcome();
  if (action === "create") await renderNewSeed();
  if (action === "import") renderImport();
  if (action === "watch") renderWatchOnly();
  if (action === "seed-saved") renderPasswordSetup();
  if (action === "copy-seed") { await navigator.clipboard.writeText(app.dataset.pendingSeed); toast("Seed copied — clear your clipboard after use"); }
  if (action === "copy-address") { await navigator.clipboard.writeText(state.address); toast("Address copied"); }
  if (action === "migrate") {
    button.disabled = true;
    button.innerHTML = '<span class="spinner"></span> Preparing…';
    try {
      await ensureQuantumCrypto();
      const built = JSON.parse(buildQuantumMigration(JSON.stringify({
        seed: state.wallet.seedPhrase,
        chainId: state.status.chain_id,
        genesisHash: state.status.genesis_hash,
        destinationIndex: state.addressIndex,
        feeRate: String(state.status.default_fee_per_byte || 1),
        signingHeight: Number(state.status.height || 0) + 1,
        authorizationAggregationActivationHeight: Number(state.status.transaction_v2_authorization_aggregation_activation_height ?? Number.MAX_SAFE_INTEGER),
        utxos: state.legacyUtxos,
      })));
      const amount = state.legacyUtxos.reduce((sum, utxo) => sum + BigInt(utxo.output.amount), 0n) - BigInt(built.fee);
      renderConfirmation(built, BigInt(built.fee), state.address, amount);
    } catch (error) {
      toast(error.message || String(error), true);
      button.disabled = false;
      button.textContent = "Migrate now";
    }
    return;
  }
  if (action === "retry") await openWallet();
  if (action === "wallets") { window.clearInterval(state.timer); state.wallet = null; state.walletMeta = null; state.view = "home"; renderWalletPicker(); }
  if (action === "lock") {
    window.clearInterval(state.timer); state.wallet = null; state.walletMeta = null; state.view = "home";
    if (activeWallet()?.type === "readonly") renderWalletPicker(); else renderLock();
  }
  if (action === "forget") {
    const warning = state.walletMeta?.type === "readonly"
      ? `Remove “${state.walletMeta.name}” from this device?`
      : `Remove “${state.walletMeta?.name}” from this device? Make sure you have saved the seed.`;
    if (window.confirm(warning)) {
      saveStore(removeWallet(state.store, state.walletMeta.id));
      window.clearInterval(state.timer); state.wallet = null; state.walletMeta = null; state.view = "home";
      state.store.wallets.length ? renderWalletPicker() : renderWelcome();
    }
  }
});

app.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.target;
  const button = form.querySelector('[type="submit"]');
  button.disabled = true;
  const original = button.textContent;
  button.innerHTML = '<span class="spinner"></span> Please wait…';
  try {
    if (form.id === "unlock-form") {
      const meta = activeWallet();
      state.wallet = await decryptWallet(meta.record, form.password.value);
      state.walletMeta = meta;
      await openWallet();
    } else if (form.id === "import-form") {
      const seed = await validateMnemonic(form.seed.value);
      await saveAndOpen(seed, form["new-password"].value, form["import-name"].value);
    } else if (form.id === "create-form") {
      if (form["create-password"].value !== form["confirm-password"].value) throw new Error("The passwords do not match");
      await saveAndOpen(app.dataset.pendingSeed, form["create-password"].value, form["create-name"].value);
    } else if (form.id === "watch-form") {
      const address = form["watch-address"].value.trim().toLowerCase();
      const walletName = form["wallet-name"].value.trim();
      if (!walletName) throw new Error("Enter a wallet name");
      const decoded = decodeVersionedAddress(address, "iuna");
      const publicKeyHex = decoded.payloadHex;
      if (state.store.wallets.some((wallet) => (wallet.address || "").toLowerCase() === address || (!wallet.address && wallet.publicKeyHex === publicKeyHex))) throw new Error("This wallet is already on this device");
      const meta = { id: `watch-${decoded.version}-${publicKeyHex}`, name: walletName, type: "readonly", publicKeyHex, address };
      saveStore(upsertWallet(state.store, meta));
      await openStoredWallet(meta);
    } else if (form.id === "send-form") {
      if (state.walletMeta?.type === "readonly") throw new Error("Watch-only wallets cannot sign transactions");
      const amount = parseIuna(form.amount.value);
      const feeRate = parseFeeRate(form["fee-rate"].value);
      if (state.status.transaction_v2_active) {
        await ensureQuantumCrypto();
        const built = JSON.parse(buildQuantumTransfer(JSON.stringify({
          seed: state.wallet.seedPhrase,
          chainId: state.status.chain_id,
          genesisHash: state.status.genesis_hash,
          recipientAddress: form.recipient.value.trim(),
          amount: amount.toString(),
          feeRate: feeRate.toString(),
          changeIndex: state.addressIndex,
          signingHeight: Number(state.status.height || 0) + 1,
          authorizationAggregationActivationHeight: Number(state.status.transaction_v2_authorization_aggregation_activation_height ?? Number.MAX_SAFE_INTEGER),
          utxos: state.hybridUtxos,
        })));
        renderConfirmation(built, BigInt(built.fee), form.recipient.value.trim(), amount);
      } else {
        const built = await buildSignedTransfer({ wallet: state.wallet, status: state.status, utxos: state.utxos, recipientAddress: form.recipient.value, amount, feeRate });
        renderConfirmation(built.transaction, built.fee, form.recipient.value.trim(), amount);
      }
    }
  } catch (error) {
    toast(error?.message || String(error), true); button.disabled = false; button.textContent = original;
  }
});

state.store = loadStore();

if (!window.isSecureContext || !crypto?.subtle) {
  app.innerHTML = '<section class="center-card"><div><h1>Secure connection required</h1><p>Open this wallet over HTTPS or localhost.</p></div></section>';
} else if (state.store.wallets.length) renderLock(); else renderWelcome();
