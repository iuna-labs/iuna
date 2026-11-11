import { generateMnemonic, mnemonicEntropy, validateMnemonic } from "./mnemonic.js";
import {
  apiEndpoint, buildSignedTransaction, decryptWallet, defaultApiEndpoint, encryptWallet, fetchWalletView,
  fetchWalletActivity, formatIuna, parseIuna, submitTransaction, walletFromEntropy,
} from "./wallet-core.js";

const app = document.querySelector("#app");
const toastElement = document.querySelector("#toast");
const WALLETS_KEY = "iunaWallets";
const LEGACY_WALLET_KEY = "iunaEd25519Wallet";
const ENDPOINT_KEY = "iunaWalletApi";
// Wallets of the previous network. Their records stay untouched; this version cannot read them.
const PREVIOUS_NETWORK_KEYS = ["iuna.wallets.v2", "iuna.wallet.v1"];
const CHECK_COUNT = 3;
const IDLE_LOCK_MS = 10 * 60 * 1000;
const POLL_MS = 5000;
let checkIndexes = [];
let pendingSeed = "";
let wallet = null;
let view = null;
let pendingTransfer = null;
let pollTimer = null;
let idleTimer = null;
let awayFromDashboard = false;
let dashboardTab = "overview";
let activityBusy = false;
let activityPageDepth = 1;
// After a failed page load, automatic loading pauses until the next poll.
let activityRetryAt = 0;
let activityObserver = null;
const activityFilters = { transfers: true, tickets: true, heartbeatRewards: true, ledgerRewards: true };

// Storage: { selected: publicKey, wallets: [{ name, record }] }, each record encrypted on its own.
function readJson(key, fallback) { try { return JSON.parse(localStorage.getItem(key)) ?? fallback; } catch { return fallback; } }
function writeJson(key, value) { localStorage.setItem(key, JSON.stringify(value)); }
function store() {
  const stored = readJson(WALLETS_KEY, null);
  if (stored?.wallets) return stored;
  // One-time migration from the single-wallet layout.
  const legacy = readJson(LEGACY_WALLET_KEY, null);
  const migrated = { selected: legacy?.publicKey || "", wallets: legacy ? [{ name: "Wallet 1", record: legacy }] : [] };
  if (legacy) { writeJson(WALLETS_KEY, migrated); localStorage.removeItem(LEGACY_WALLET_KEY); }
  return migrated;
}
function selectedEntry() { const { selected, wallets } = store(); return wallets.find((entry) => entry.record.publicKey === selected) || wallets[0] || null; }
function shell(content, controls = "") { return `<div class="top"><div class="brand"><img src="./icon.svg" alt=""><span>iuna wallet</span></div><div class="actions tight">${controls}</div></div><section class="card">${content}</section>`; }
function toast(message, error = false) { toastElement.textContent = message; toastElement.className = `toast show${error ? " error" : ""}`; clearTimeout(toastElement.timer); toastElement.timer = setTimeout(() => { toastElement.className = "toast"; }, 4000); }
function escape(value) { return String(value).replace(/[&<>"']/g, (character) => ({ "&":"&amp;", "<":"&lt;", ">":"&gt;", '"':"&quot;", "'":"&#39;" })[character]); }
function words(seed) { return seed.split(" ").map((word) => `<div class="seed-word">${escape(word)}</div>`).join(""); }
function short(value) { const text = String(value || ""); return text.length > 18 ? `${text.slice(0, 9)}…${text.slice(-7)}` : text; }
function timeLabel(seconds) { return new Date(seconds * 1000).toLocaleString(); }
function relativeTime(seconds) {
  const elapsed = Math.max(0, Math.floor(Date.now() / 1000) - Number(seconds));
  if (!Number.isFinite(elapsed)) return "";
  if (elapsed < 60) return `${elapsed}s ago`;
  if (elapsed < 3600) return `${Math.floor(elapsed / 60)}m ago`;
  if (elapsed < 86400) return `${Math.floor(elapsed / 3600)}h ago`;
  return `${Math.floor(elapsed / 86400)}d ago`;
}
function randomCheckIndexes() {
  const indexes = new Set();
  while (indexes.size < CHECK_COUNT) indexes.add(1 + (crypto.getRandomValues(new Uint32Array(1))[0] % 24));
  return [...indexes].sort((left, right) => left - right);
}
async function copyText(text) {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text);
  const field = Object.assign(document.createElement("textarea"), { value: text, readOnly: true });
  document.body.append(field); field.select();
  const copied = document.execCommand("copy"); field.remove();
  if (!copied) throw new Error("Copying is unavailable; select the address manually");
}

function previousNetworkNotice() {
  if (!PREVIOUS_NETWORK_KEYS.some((key) => localStorage.getItem(key))) return "";
  return `<p class="muted">This browser still holds a wallet from the previous iuna network. This version cannot open it, and its balance did not carry over to the Heartbeat and Ledger network.</p>`;
}

function renderWelcome() {
  const back = store().wallets.length ? `<button data-action="unlock-screen">Back to wallets</button>` : "";
  app.innerHTML = shell(`<h1>Add an iuna wallet</h1><p>Create a new Ed25519 wallet for the Heartbeat and Ledger network, or restore one from its 24-word recovery phrase.</p>${previousNetworkNotice()}<div class="choice"><button data-action="create"><b>Create a new wallet</b><span>Generate and verify a new recovery phrase.</span></button><button data-action="restore"><b>Restore an existing wallet</b><span>Use your existing 24 words.</span></button></div><p class="muted">Seeds are encrypted in this browser and never sent to the node.</p>`, `${back}<button data-action="settings">Settings</button>`);
}

function renderUnlock() {
  const { wallets } = store();
  const selected = selectedEntry();
  const options = wallets.map((entry) => `<option value="${escape(entry.record.publicKey)}"${entry === selected ? " selected" : ""}>${escape(entry.name)} · ${escape(short(entry.record.publicKey))}</option>`).join("");
  app.innerHTML = shell(`<h1>Unlock wallet</h1><form id="unlock" class="stack"><label>Wallet<select name="wallet">${options}</select></label><label>Password<input name="password" type="password" autocomplete="current-password" required></label><button class="primary" type="submit">Unlock</button></form><div class="actions"><button data-action="welcome">Add another wallet</button><button data-action="forget">Remove selected wallet</button></div>`, `<button data-action="settings">Settings</button>`);
}

function renderSettings() {
  if (wallet && view) { renderDashboard("settings"); return; }
  const configured = localStorage.getItem(ENDPOINT_KEY) || defaultApiEndpoint();
  app.innerHTML = shell(settingsScreen(configured), `<button data-action="${store().wallets.length ? "unlock-screen" : "welcome"}">Back</button>`);
}

async function renderNewSeed() {
  pendingSeed = await generateMnemonic();
  renderSeed();
}

function renderSeed() {
  app.innerHTML = shell(`<h1>Save your recovery phrase</h1><div class="warning"><b>This is the only wallet backup.</b> Write all 24 words down in order. Never share them and do not take a screenshot.</div><div class="seed-grid">${words(pendingSeed)}</div><button class="primary" data-action="seed-saved">I wrote down all 24 words</button>`, `<button data-action="welcome">Back</button>`);
}

function renderSeedCheck() {
  checkIndexes = randomCheckIndexes();
  const fields = checkIndexes.map((index) => `<label>Word ${index}<input name="word-${index}" autocomplete="off" autocapitalize="none" spellcheck="false" required></label>`).join("");
  app.innerHTML = shell(`<h1>Check your recovery phrase</h1><p>Enter the requested words from your paper backup.</p><form id="verify" class="stack">${fields}<button class="primary" type="submit">Verify recovery phrase</button></form>`, `<button data-action="show-seed">Back</button>`);
}

function nameField() { return `<label>Wallet name<input name="name" maxlength="40" value="Wallet ${store().wallets.length + 1}" required></label>`; }

function renderPassword() {
  app.innerHTML = shell(`<h1>Protect this device</h1><p>This password encrypts the wallet stored in this browser.</p><form id="create-wallet" class="stack">${nameField()}<label>Password<input name="password" type="password" minlength="10" autocomplete="new-password" required></label><label>Repeat password<input name="confirmation" type="password" minlength="10" autocomplete="new-password" required></label><button class="primary" type="submit">Create wallet</button></form>`);
}

function renderRestore() {
  app.innerHTML = shell(`<h1>Restore existing wallet</h1><form id="restore-wallet" class="stack"><label>24-word recovery phrase<textarea name="seed" autocomplete="off" autocapitalize="none" spellcheck="false" required></textarea></label>${nameField()}<label>New local password<input name="password" type="password" minlength="10" autocomplete="new-password" required></label><label>Repeat password<input name="confirmation" type="password" minlength="10" autocomplete="new-password" required></label><button class="primary" type="submit">Restore wallet</button></form>`, `<button data-action="welcome">Back</button>`);
}

function renderBackupUnlock() {
  app.innerHTML = shell(`<h1>View recovery phrase</h1><p>Enter your wallet password to show the 24 words.</p><form id="backup" class="stack"><label>Password<input name="password" type="password" autocomplete="current-password" required></label><button class="primary" type="submit">Show recovery phrase</button></form>`, `<button data-action="dashboard">Back</button>`);
}

function renderBackup(seedPhrase) {
  app.innerHTML = shell(`<h1>Recovery phrase</h1><div class="warning">Anyone with these words controls this wallet. Keep them offline and private.</div><div class="seed-grid">${words(seedPhrase)}</div><button data-action="dashboard">Done</button>`);
}

function renderConfirm() {
  const spendable = Number(view.spendable_balance) - pendingTransfer.amount - pendingTransfer.fee;
  app.innerHTML = shell(`<h1>Confirm transfer</h1><p>Check the full recipient address. Ledger transactions cannot be reversed.</p><div class="address"><span class="label">Recipient</span><code>${escape(pendingTransfer.recipient)}</code></div><div class="stats"><div class="stat"><span>Amount</span><strong>${escape(formatIuna(pendingTransfer.amount))} IUNA</strong></div><div class="stat"><span>Fee</span><strong>${escape(formatIuna(pendingTransfer.fee))} IUNA</strong></div><div class="stat"><span>Spendable after</span><strong>${escape(formatIuna(Math.max(0, spendable)))} IUNA</strong></div></div><div class="actions"><button data-action="cancel-transfer">Cancel</button><button class="primary" data-action="confirm-transfer">Sign and send</button></div>`);
}

const ACTIVITY_LABELS = {
  sent: "Sent", received: "Received", self: "To yourself", ticket: "Mining ticket",
  heartbeat_reward: "Heartbeat reward", ledger_reward: "Ledger reward",
};

// Same two-letter codes and colours as the node app; the filters double as their legend.
const ACTIVITY_CODES = { transfers: "TX", tickets: "TI", heartbeatRewards: "HR", ledgerRewards: "LR" };

function activityType(item) {
  if (["sent", "received", "self"].includes(item?.kind)) return "transfers";
  if (item?.kind === "ticket") return "tickets";
  if (item?.kind === "heartbeat_reward") return "heartbeatRewards";
  if (item?.kind === "ledger_reward") return "ledgerRewards";
  return "transfers";
}

function activityRows() {
  const rows = [
    ...(view?.activity?.pending || []).map((item) => ({ ...item, pending: true })),
    ...(view?.activity?.confirmed || []).map((item) => ({ ...item, pending: false })),
  ].filter((item) => activityFilters[activityType(item)]);
  if (!rows.length && view?.activity?.next_before_height == null) return `<p class="muted">No activity matches these filters.</p>`;
  const list = rows.map((row) => {
    const incoming = ["received", "self", "heartbeat_reward", "ledger_reward"].includes(row.kind);
    const type = activityType(row);
    const label = ACTIVITY_LABELS[row.kind] || row.kind;
    const meta = row.pending ? "Waiting for confirmation" : [
      row.heartbeat_height == null ? "" : `Heartbeat #${row.heartbeat_height}`,
      row.timestamp == null ? "" : timeLabel(row.timestamp),
    ].filter(Boolean).join(" · ") || "Confirmed";
    return `<div class="activity-row${row.pending ? " pending" : ""}"><span class="pill activity-code ${type}" title="${escape(label)}" aria-hidden="true">${ACTIVITY_CODES[type]}</span><div class="activity-copy"><div class="activity-title">${escape(label)}</div><div class="activity-meta">${escape(meta)}</div></div><b class="${incoming ? "incoming" : ""}">${incoming ? "+" : "−"} ${escape(formatIuna(row.amount))} IUNA</b></div>`;
  }).join("");
  const more = view?.activity?.next_before_height == null ? "" : `<div class="activity-more" data-field="activity-more" role="status" aria-label="Loading older activity"><span class="spinner" aria-hidden="true"></span></div>`;
  return `${list ? `<div class="activity">${list}</div>` : ""}${more}`;
}

function heartbeatLabel() {
  const age = relativeTime(view?.tip_timestamp);
  return age ? `${view.heartbeat_height} (${age})` : String(view?.heartbeat_height ?? "—");
}

function walletNav() {
  const tabs = [["overview", "Overview"], ["receive", "Receive"], ["send", "Send"], ["activity", "Activity"], ["settings", "Settings"]];
  return `<nav class="bottom-nav" aria-label="Wallet">${tabs.map(([tab, label]) => `<button class="nav-item${dashboardTab === tab ? " active" : ""}" data-action="wallet-tab" data-tab="${tab}"${dashboardTab === tab ? ' aria-current="page"' : ""}><span aria-hidden="true">${{ overview: "⌂", receive: "↓", send: "↑", activity: "↕", settings: "⚙" }[tab]}</span>${label}</button>`).join("")}</nav>`;
}

function overviewScreen(entry) {
  return `<h1>${escape(entry?.name || "Wallet")}</h1><div class="stats"><div class="stat"><span>Balance</span><strong data-field="balance">${escape(formatIuna(view.balance))} IUNA</strong></div><div class="stat"><span>Spendable</span><strong data-field="spendable">${escape(formatIuna(view.spendable_balance))} IUNA</strong></div><div class="stat"><span>Heartbeat</span><strong data-field="heartbeat">${escape(heartbeatLabel())}</strong></div></div><p class="muted" data-field="connection">${escape(connectionLabel())}</p>`;
}

function receiveScreen() {
  const receiveAddress = view.receive_address || "Unavailable while disconnected";
  return `<h1>Receive IUNA</h1><p>Share this address with the sender. A new receive address is selected after use.</p><div class="address"><code data-field="receive-address">${escape(receiveAddress)}</code></div><div class="actions"><button class="primary" data-action="copy" data-field="copy-receive" data-value="${escape(view.receive_address || "")}"${view.receive_address ? "" : " disabled"}>Copy address</button><button data-action="backup">View recovery phrase</button></div>`;
}

function sendScreen() {
  return `<h1>Send IUNA</h1><form id="send" class="send"><label>Recipient address<input name="recipient" minlength="64" maxlength="64" autocomplete="off" autocapitalize="none" spellcheck="false" required></label><div class="row"><label>Amount<span class="amount-row"><input name="amount" inputmode="decimal" placeholder="0.00" required><button type="button" data-action="max">Max</button></span></label><label>Fee<input name="fee" inputmode="decimal" value="0.00000001" required></label></div><button class="primary" type="submit">Review</button></form>`;
}

function activityScreen() {
  const filters = [["transfers", "Transfers"], ["tickets", "Tickets"], ["heartbeatRewards", "Heartbeat rewards"], ["ledgerRewards", "Ledger rewards"]]
    .map(([type, label]) => `<button class="activity-filter ${type}${activityFilters[type] ? " active" : ""}" data-action="activity-filter" data-filter="${type}" aria-pressed="${activityFilters[type]}">${label}</button>`).join("");
  return `<h1>Activity</h1><p>Confirmed and pending wallet activity.</p><div class="activity-filters" aria-label="Activity filters">${filters}</div><div data-field="activity">${activityRows()}</div>`;
}

function settingsScreen(configured = localStorage.getItem(ENDPOINT_KEY) || defaultApiEndpoint()) {
  return `<h1>Settings</h1><form id="endpoint" class="stack settings-form"><label>Node endpoint<input name="endpoint" type="url" value="${escape(configured)}" autocomplete="off" spellcheck="false"></label><div class="actions"><button class="primary" type="submit">Save and test</button><button type="button" data-action="reset-endpoint">Use default</button></div></form>`;
}

function renderDashboard(tab = dashboardTab) {
  if (!view) return;
  dashboardTab = ["overview", "receive", "send", "activity", "settings"].includes(tab) ? tab : "overview";
  awayFromDashboard = false;
  const entry = selectedEntry();
  const content = {
    overview: () => overviewScreen(entry),
    receive: receiveScreen,
    send: sendScreen,
    activity: activityScreen,
    settings: settingsScreen,
  }[dashboardTab]();
  app.innerHTML = `${shell(`<div data-screen="wallet">${content}</div>`, `<button data-action="lock">Lock</button>`)}${walletNav()}`;
  observeActivityEnd();
}

function connectionLabel() { return `Connected to ${apiEndpoint()} · ${view.network} · ${view.ledger_blocks} Ledger blocks`; }

// Polling only updates the live values, so it never clears the send form or
// replaces another screen such as the recovery phrase.
function updateDashboard() {
  if (!view) return;
  const fields = {
    balance: `${formatIuna(view.balance)} IUNA`,
    spendable: `${formatIuna(view.spendable_balance)} IUNA`,
    heartbeat: heartbeatLabel(),
    connection: connectionLabel(),
  };
  for (const [name, value] of Object.entries(fields)) {
    const field = app.querySelector(`[data-field=${name}]`);
    if (field) field.textContent = value;
  }
  const address = app.querySelector("[data-field=receive-address]");
  if (address) address.textContent = view.receive_address || "Unavailable while disconnected";
  const copy = app.querySelector("[data-field=copy-receive]");
  if (copy) { copy.dataset.value = view.receive_address || ""; copy.disabled = !view.receive_address; }
  const activity = app.querySelector("[data-field=activity]");
  if (activity) { activity.innerHTML = activityRows(); observeActivityEnd(); }
}

// Infinite scroll: the spinner at the end of the list loads the next page once
// it comes near the viewport. Observing a freshly rendered spinner reports its
// current visibility, so pages keep loading while the end stays in view.
function observeActivityEnd() {
  activityObserver?.disconnect();
  const end = app.querySelector("[data-field=activity-more]");
  if (!end) return;
  activityObserver ||= new IntersectionObserver((entries) => {
    if (!entries.some((entry) => entry.isIntersecting) || Date.now() < activityRetryAt) return;
    loadOlderActivity().catch((error) => { activityRetryAt = Date.now() + POLL_MS; toast(error.message, true); });
  }, { rootMargin: "0px 0px 300px 0px" });
  activityObserver.observe(end);
}

function mergeLoadedActivity(fresh, previous) {
  if (activityPageDepth <= 1 || !fresh?.activity || !previous?.activity) return;
  const seen = new Set(fresh.activity.confirmed.map((item) => `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`));
  fresh.activity.confirmed.push(...previous.activity.confirmed.filter((item) => {
    const key = `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  }));
  fresh.activity.next_before_height = previous.activity.next_before_height;
}

async function loadOlderActivity() {
  const before = view?.activity?.next_before_height;
  if (before == null || activityBusy) return;
  activityBusy = true;
  const owner = wallet;
  try {
    const page = await fetchWalletActivity(
      apiEndpoint(),
      wallet.publicKeyHex,
      view.activity_addresses,
      before,
    );
    // The wallet may have been locked or switched while the page was loading.
    if (wallet !== owner || !view?.activity) return;
    const seen = new Set(view.activity.confirmed.map((item) => `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`));
    view.activity.confirmed.push(...page.confirmed.filter((item) => {
      const key = `${item.kind}:${item.id}:${item.heartbeat_height ?? ""}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    }));
    view.activity.next_before_height = page.next_before_height;
    activityPageDepth += 1;
  } finally {
    activityBusy = false;
    updateDashboard();
  }
}

function showScreen(render) { awayFromDashboard = true; render(); }

async function save(seedPhrase, password, name) {
  const derived = await walletFromEntropy(await mnemonicEntropy(seedPhrase));
  const record = await encryptWallet(seedPhrase, password, derived.publicKeyHex);
  const stored = store();
  if (stored.wallets.some((entry) => entry.record.publicKey === derived.publicKeyHex)) throw new Error("This wallet is already in this browser");
  stored.wallets.push({ name: name.trim() || `Wallet ${stored.wallets.length + 1}`, record });
  stored.selected = derived.publicKeyHex;
  writeJson(WALLETS_KEY, stored);
  unlocked(derived);
  pendingSeed = "";
  return refresh();
}

function unlocked({ seedPhrase: _seedPhrase, ...derived }) {
  // The recovery phrase is only decrypted again on demand; keep the signing key only.
  wallet = derived;
  dashboardTab = "overview";
  activityPageDepth = 1;
  awayFromDashboard = false;
  resetIdleTimer();
}

function lock() {
  activityObserver?.disconnect();
  wallet = null; view = null; pendingSeed = ""; pendingTransfer = null; awayFromDashboard = false;
  clearTimeout(pollTimer); clearTimeout(idleTimer);
  renderUnlock();
}

function resetIdleTimer() {
  if (!wallet) return;
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => { lock(); toast("Wallet locked after inactivity"); }, IDLE_LOCK_MS);
}

async function refresh(silent = false) {
  if (!wallet) return;
  if (document.hidden) return true;
  let connected = true;
  const render = () => {
    if (awayFromDashboard) return;
    if (app.querySelector("[data-screen=wallet]")) updateDashboard(); else renderDashboard();
  };
  try {
    const fresh = await fetchWalletView(apiEndpoint(), wallet);
    mergeLoadedActivity(fresh, view);
    view = fresh;
    if (wallet) render();
  }
  catch (error) {
    connected = false;
    view ||= { network: "Disconnected", heartbeat_height: "—", ledger_blocks: "—", balance: 0, spendable_balance: 0, utxos: [], activity: { pending: [], confirmed: [] } };
    if (wallet) render();
    if (!silent) toast(error.message, true);
  }
  clearTimeout(pollTimer); pollTimer = setTimeout(() => refresh(true), POLL_MS);
  return connected;
}

async function sendPendingTransfer() {
  const transfer = pendingTransfer;
  const transaction = await buildSignedTransaction({ wallet, utxos: view.utxos, changeIndex: view.change_index, ...transfer });
  const result = await submitTransaction(apiEndpoint(), transaction);
  // A second send before the next refresh must not reuse this change address.
  if (transaction.outputs.length > 1) view.change_index = (view.change_index || 0) + 1;
  pendingTransfer = null;
  toast(`Transaction submitted: ${result.id.slice(0, 12)}…`);
  await refresh(true);
  renderDashboard("activity");
}

app.addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-action]"); if (!button) return;
  try {
    const action = button.dataset.action;
    if (action === "create") await renderNewSeed();
    if (action === "restore") renderRestore();
    if (action === "welcome") renderWelcome();
    if (action === "unlock-screen") renderUnlock();
    if (action === "settings") showScreen(renderSettings);
    if (action === "wallet-tab") renderDashboard(button.dataset.tab);
    if (action === "activity-filter") { activityFilters[button.dataset.filter] = !activityFilters[button.dataset.filter]; renderDashboard("activity"); }
    if (action === "seed-saved") renderSeedCheck();
    if (action === "show-seed") renderSeed();
    if (action === "dashboard") renderDashboard();
    if (action === "backup") showScreen(renderBackupUnlock);
    if (action === "copy") { await copyText(button.dataset.value); toast("Copied"); }
    if (action === "max") {
      const fee = parseIuna(app.querySelector("#send [name=fee]").value);
      const maximum = Number(view.spendable_balance) - fee;
      if (maximum <= 0) throw new Error("The spendable balance does not cover the fee");
      app.querySelector("#send [name=amount]").value = formatIuna(maximum);
    }
    if (action === "cancel-transfer") { pendingTransfer = null; renderDashboard("send"); }
    if (action === "confirm-transfer") { button.disabled = true; await sendPendingTransfer(); }
    if (action === "reset-endpoint") { localStorage.removeItem(ENDPOINT_KEY); toast(`Using ${apiEndpoint()}`); renderSettings(); }
    if (action === "lock") lock();
    if (action === "forget") {
      const stored = store();
      const selected = app.querySelector("#unlock [name=wallet]")?.value;
      const entry = stored.wallets.find((item) => item.record.publicKey === selected);
      if (entry && confirm(`Remove "${entry.name}" from this browser? Make sure its recovery phrase is backed up.`)) {
        stored.wallets = stored.wallets.filter((item) => item !== entry);
        stored.selected = stored.wallets[0]?.record.publicKey || "";
        writeJson(WALLETS_KEY, stored);
        stored.wallets.length ? renderUnlock() : renderWelcome();
      }
    }
  } catch (error) {
    if (button.dataset.action === "confirm-transfer") button.disabled = false;
    toast(error.message, true);
  }
});

app.addEventListener("submit", async (event) => {
  event.preventDefault(); const form = event.target; const submit = form.querySelector("button[type=submit]"); submit.disabled = true;
  try {
    if (form.id === "verify") {
      const seedWords = pendingSeed.split(" ");
      if (!checkIndexes.every((index) => form.elements[`word-${index}`].value.trim().toLowerCase() === seedWords[index - 1])) throw new Error("Those words do not match your recovery phrase");
      renderPassword(); return;
    }
    if (form.id === "create-wallet") {
      if (form.password.value !== form.confirmation.value) throw new Error("The passwords do not match");
      const connected = await save(pendingSeed, form.password.value, form.elements.name.value); toast(connected ? "Wallet created" : "Wallet created; node is unavailable", !connected); return;
    }
    if (form.id === "restore-wallet") {
      if (form.password.value !== form.confirmation.value) throw new Error("The passwords do not match");
      const seed = await validateMnemonic(form.seed.value); const connected = await save(seed, form.password.value, form.elements.name.value); toast(connected ? "Wallet restored" : "Wallet restored; node is unavailable", !connected); return;
    }
    if (form.id === "unlock") {
      const stored = store();
      stored.selected = form.elements.wallet.value;
      writeJson(WALLETS_KEY, stored);
      unlocked(await decryptWallet(selectedEntry().record, form.password.value, mnemonicEntropy)); await refresh(); return;
    }
    if (form.id === "backup") {
      const { seedPhrase } = await decryptWallet(selectedEntry().record, form.password.value, mnemonicEntropy);
      renderBackup(seedPhrase); return;
    }
    if (form.id === "endpoint") {
      const value = form.endpoint.value.trim().replace(/\/$/, "");
      if (value && !/^https?:\/\/[^\s/]+$/.test(value)) throw new Error("Use an http(s) origin such as http://127.0.0.1:18662, without a path");
      if (value) localStorage.setItem(ENDPOINT_KEY, value); else localStorage.removeItem(ENDPOINT_KEY);
      if (wallet) {
        const connected = await refresh(true);
        toast(connected ? `Connected to ${apiEndpoint()}` : `Saved, but ${apiEndpoint()} did not answer`, !connected);
      } else {
        toast(`Saved ${apiEndpoint()}`);
      }
      return;
    }
    if (form.id === "send") {
      const recipient = form.recipient.value.trim().toLowerCase();
      if (!/^[0-9a-f]{64}$/.test(recipient)) throw new Error("The recipient must be a 64-character address");
      const amount = parseIuna(form.amount.value);
      const fee = parseIuna(form.fee.value);
      if (amount <= 0) throw new Error("Enter an amount above zero");
      if (amount + fee > Number(view.spendable_balance)) throw new Error("Amount plus fee exceeds the spendable balance");
      pendingTransfer = { recipient, amount, fee };
      showScreen(renderConfirm);
      return;
    }
  } catch (error) { toast(error.message, true); }
  finally { submit.disabled = false; }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && pendingTransfer) { pendingTransfer = null; renderDashboard("send"); }
});
for (const type of ["pointerdown", "keydown"]) document.addEventListener(type, resetIdleTimer, { passive: true });
document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(true); });

store().wallets.length ? renderUnlock() : renderWelcome();
