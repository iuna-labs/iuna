import {
  API_BASE, LEGACY_STORAGE_KEY, STORAGE_KEY, api, buildSignedTransfer, decodeAddress,
  decryptWallet, encodeAddress, encryptWallet, formatIuna, normalizeWalletStore,
  parseIuna, removeWallet, upsertWallet, walletFromSeed, walletId,
} from "./wallet-core.js";
import { generateMnemonic, validateMnemonic } from "./mnemonic.js";

const app = document.querySelector("#app");
const toastElement = document.querySelector("#toast");
const state = { store: null, wallet: null, walletMeta: null, status: null, address: "", balance: null, utxos: [], transactions: [], view: "home", timer: null };
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
    ? { type: "readonly", publicKeyHex: meta.publicKeyHex, publicKey: Uint8Array.from(meta.publicKeyHex.match(/.{2}/g), (pair) => Number.parseInt(pair, 16)) }
    : state.wallet;
  await openWallet();
}

async function fetchWalletData() {
  state.status = await api("/status");
  state.address = encodeAddress(state.wallet.publicKey, state.status.chain_id);
  const encoded = encodeURIComponent(state.address);
  const [balance, utxos, transactions] = await Promise.all([
    api(`/addresses/${encoded}/balance`),
    api(`/addresses/${encoded}/utxos`),
    api(`/addresses/${encoded}/transactions?limit=50`),
  ]);
  state.balance = balance;
  state.utxos = utxos.utxos || [];
  state.transactions = transactions.items || [];
}

async function openWallet() {
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
  const owner = state.wallet.publicKeyHex;
  if (item.kind === "reward") {
    const amount = tx.outputs?.find((output) => output.address === owner)?.amount || 0;
    return { title: "Block reward", incoming: true, amount, symbol: "★" };
  }
  if (tx.kind === "mine") return { title: "Mining reward", incoming: true, amount: 1_000_000, symbol: "✦" };
  if (tx.kind === "burn") return { title: "Burn", incoming: false, amount: tx.amount || 0, symbol: "×" };
  const sent = tx.inputs?.some((input) => input.owner === owner);
  const relevant = tx.outputs?.filter((output) => sent ? output.address !== owner : output.address === owner) || [];
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

function activityList(limit) {
  const items = state.transactions.slice(0, limit);
  if (!items.length) return '<div class="empty">No transactions yet.<br>Your new wallet is ready to use.</div>';
  return `<div class="activity-list">${items.map((item) => {
    const info = transactionInfo(item);
    const date = formatTransactionDate(item);
    return `<div class="activity"><div class="activity-icon">${info.symbol}</div><div><div class="activity-title">${info.title}</div><div class="activity-meta">${escapeHtml(date)} · ${escapeHtml(item.status)}</div></div><div class="activity-amount ${info.incoming ? "in" : ""}">${info.incoming ? "+" : "−"}${formatIuna(info.amount, 4)} IUNA</div></div>`;
  }).join("")}</div>`;
}

function renderHome() {
  const readonly = state.walletMeta?.type === "readonly";
  return `${topbar()}<section>${readonly ? '<div class="mode-badge">Watch-only · signing disabled</div>' : ""}<p class="eyebrow">Available balance</p><h1 class="balance">${formatIuna(state.balance?.spendable, 6)} <span>IUNA</span></h1><p class="subbalance">${formatIuna(state.balance?.confirmed, 6)} confirmed · block ${escapeHtml(state.balance?.height)}</p><div class="actions"><button class="button" data-view="send" ${readonly ? "disabled" : ""}>${icon("send")} Send</button><button class="button secondary" data-view="receive">${icon("receive")} Receive</button></div><div class="section-head"><h2>Recent activity</h2><button data-view="activity">View all</button></div><div class="panel">${activityList(5)}</div></section>`;
}

function renderSend() {
  if (state.walletMeta?.type === "readonly") return `${topbar()}<p class="eyebrow">Watch-only</p><h1 class="view-title">Sending is disabled.</h1><p class="view-copy">This wallet contains no seed or private key, so it cannot sign transactions.</p><button class="button secondary" data-view="home" style="width:100%">Back to overview</button>`;
  return `${topbar()}<p class="eyebrow">Transaction</p><h1 class="view-title">Send IUNA</h1><p class="view-copy">The transaction is signed on this device.</p><form id="send-form" class="panel send-card"><div class="field"><label for="recipient">Recipient</label><input id="recipient" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="iuna1q…" required></div><div class="field"><label for="amount">Amount</label><div class="amount-wrap"><input id="amount" inputmode="decimal" placeholder="0.00" required><span>IUNA</span></div></div><div class="fee-line"><span>Available</span><strong>${formatIuna(state.balance?.spendable)} IUNA</strong></div><div class="fee-line"><span>Fee rate</span><strong>${escapeHtml(state.status.default_fee_per_byte)} µIUNA / byte</strong></div><button class="button" type="submit">Review transaction</button></form>`;
}

function renderReceive() {
  return `${topbar()}<p class="eyebrow">Your address</p><h1 class="view-title">Receive IUNA</h1><p class="view-copy">Share this mainnet address with the sender.</p><div class="panel"><div class="receive-emblem">${icon("receive")}</div><p class="eyebrow">Receiving address</p><div class="address-box"><code>${escapeHtml(state.address)}</code><button class="icon-button" data-action="copy-address" aria-label="Copy address">${icon("copy")}</button></div><p class="security-note">Always verify the first and last characters when sharing an address.</p></div>`;
}

function renderActivity() {
  return `${topbar()}<p class="eyebrow">Wallet</p><h1 class="view-title">Activity</h1><p class="view-copy">Confirmed and pending transactions.</p><div class="panel">${activityList(50)}</div>`;
}

function renderSettings() {
  const security = state.walletMeta?.type === "readonly" ? "Watch-only · no private key stored" : "AES-256-GCM · PBKDF2-SHA256 · 310,000 iterations";
  return `${topbar()}<p class="eyebrow">Wallet</p><h1 class="view-title">Settings</h1><div class="panel"><div class="setting"><h3>Name</h3><p>${escapeHtml(state.walletMeta?.name)}</p></div><div class="setting"><h3>Network</h3><p>${escapeHtml(state.status.network_id)} · block ${escapeHtml(state.status.height)}</p></div><div class="setting"><h3>Public endpoint</h3><p>${escapeHtml(API_BASE)}</p></div><div class="setting"><h3>Local security</h3><p>${security}</p></div><div class="setting"><h3>Wallet address</h3><p style="word-break:break-all">${escapeHtml(state.address)}</p></div></div><button class="button secondary" data-action="wallets" style="width:100%;margin-top:12px">Switch or add wallet</button><button class="button ghost" data-action="lock" style="width:100%;margin-top:10px">Lock wallet</button><button class="button danger" data-action="forget" style="width:100%;margin-top:10px">Remove this wallet</button><p class="security-note">iuna is experimental software. Only use funds you can afford to lose.</p>`;
}

function renderApp() {
  const renderers = { home: renderHome, send: renderSend, receive: renderReceive, activity: renderActivity, settings: renderSettings };
  app.innerHTML = `${(renderers[state.view] || renderHome)()}${nav()}`;
}

function renderConfirmation(transaction, fee, recipientAddress, amount) {
  app.innerHTML = `${topbar()}<button class="back" data-view="send">← Edit</button><p class="eyebrow">Review</p><h1 class="view-title">Does everything look right?</h1><div class="panel"><div class="detail-line"><span>You send</span><strong>${formatIuna(amount)} IUNA</strong></div><div class="detail-line"><span>To</span><strong>${escapeHtml(recipientAddress.slice(0, 12))}…${escapeHtml(recipientAddress.slice(-8))}</strong></div><div class="detail-line"><span>Network fee</span><strong>${formatIuna(fee)} IUNA</strong></div><div class="detail-line"><span>Total</span><strong>${formatIuna(amount + fee)} IUNA</strong></div></div><button class="button" id="confirm-send" style="width:100%;margin-top:14px">Sign & send</button><p class="security-note">This action cannot be reversed after submission.</p>${nav()}`;
  document.querySelector("#confirm-send").addEventListener("click", async (event) => {
    const button = event.currentTarget;
    button.disabled = true; button.innerHTML = '<span class="spinner"></span> Sending…';
    try {
      const result = await api("/transactions", { method: "POST", body: JSON.stringify(transaction) });
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
      const publicKeyHex = decodeAddress(address, "iuna");
      if (state.store.wallets.some((wallet) => wallet.publicKeyHex === publicKeyHex)) throw new Error("This wallet is already on this device");
      const meta = { id: walletId(publicKeyHex), name: walletName, type: "readonly", publicKeyHex };
      saveStore(upsertWallet(state.store, meta));
      await openStoredWallet(meta);
    } else if (form.id === "send-form") {
      if (state.walletMeta?.type === "readonly") throw new Error("Watch-only wallets cannot sign transactions");
      const amount = parseIuna(form.amount.value);
      const built = await buildSignedTransfer({ wallet: state.wallet, status: state.status, utxos: state.utxos, recipientAddress: form.recipient.value, amount });
      renderConfirmation(built.transaction, built.fee, form.recipient.value.trim(), amount);
    }
  } catch (error) {
    toast(error.message, true); button.disabled = false; button.textContent = original;
  }
});

state.store = loadStore();

if (!window.isSecureContext || !crypto?.subtle) {
  app.innerHTML = '<section class="center-card"><div><h1>Secure connection required</h1><p>Open this wallet over HTTPS or localhost.</p></div></section>';
} else if (state.store.wallets.length) renderLock(); else renderWelcome();
