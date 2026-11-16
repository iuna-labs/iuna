import {
  WINDOWS, averageInterval, formatAge, formatCount, formatDuration, formatIuna, formatIunaShort,
  formatShare, normalizeNode, resolveNode, resolveWindow, share, shortAddress, statsUrl,
} from "./stats-core.js";

const REFRESH_MS = 30_000;
const VISIBLE_ROWS = 10;

const state = {
  node: resolveNode(),
  window: resolveWindow(),
  stats: null,
  error: null,
  loading: false,
  fetchedAt: null,
  expanded: new Set(),
  request: 0,
};

const app = document.getElementById("app");

const escape = (value) => String(value ?? "").replace(/[&<>"']/g, (character) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
})[character]);

function syncUrl() {
  const params = new URLSearchParams(location.search);
  if (state.window === "day") params.delete("window"); else params.set("window", state.window);
  const query = params.toString();
  history.replaceState(null, "", `${location.pathname}${query ? `?${query}` : ""}${location.hash}`);
}

async function load({ quiet = false } = {}) {
  const request = ++state.request;
  state.loading = true;
  if (!quiet) render();
  try {
    const response = await fetch(statsUrl(state.node, state.window), { cache: "no-store" });
    if (!response.ok) throw new Error(`The node answered ${response.status}`);
    const stats = await response.json();
    if (request !== state.request) return;
    state.stats = stats;
    state.error = null;
    state.fetchedAt = Date.now();
  } catch (error) {
    if (request !== state.request) return;
    state.error = error instanceof TypeError ? "The node could not be reached" : error.message;
  } finally {
    if (request === state.request) {
      state.loading = false;
      render();
    }
  }
}

function tile(label, value, detail = "") {
  return `<div class="tile"><span>${escape(label)}</span><strong>${escape(value)}</strong>${detail ? `<small>${escape(detail)}</small>` : ""}</div>`;
}

function summary(stats) {
  const nowSeconds = Date.now() / 1000;
  const mining = stats.mining;
  const interval = averageInterval(mining);
  return `<section class="tiles" aria-label="Network summary">
    ${tile("Heartbeat height", formatCount(stats.heartbeat_height), `tip ${formatAge(nowSeconds - stats.tip_timestamp)}`)}
    ${tile("Ledger blocks", formatCount(stats.ledger_blocks), `${formatCount(mining.ledger_blocks)} in window`)}
    ${tile("Supply", `${formatIunaShort(stats.minted)} IUNA`, `${formatCount(stats.holders)} holding addresses`)}
    ${tile("Active tickets", formatCount(stats.active_tickets), `${formatCount(stats.tracked_tickets)} tracked`)}
    ${tile("Locked expenditure", `${formatIunaShort(stats.locked_budget)} IUNA`, "still paying out")}
    ${tile("Heartbeat interval", formatDuration(interval), `${formatCount(mining.heartbeat_miner_count)} miners in window`)}
  </section>`;
}

function rows(id, entries, columns) {
  if (!entries.length) return `<p class="empty">Nothing here yet.</p>`;
  const expanded = state.expanded.has(id);
  const shown = expanded ? entries : entries.slice(0, VISIBLE_ROWS);
  const body = shown.map((entry, index) => {
    const fraction = columns.share(entry);
    return `<tr>
      <td class="rank">${index + 1}</td>
      <td class="who"><button class="address" data-copy="${escape(entry.address)}" title="${escape(entry.address)}">${escape(shortAddress(entry.address))}</button>
        <span class="bar" aria-hidden="true"><i data-share="${fraction.toFixed(4)}"></i></span></td>
      <td class="num">${escape(columns.primary(entry))}<small>${escape(formatShare(fraction))}</small></td>
      <td class="num secondary">${escape(columns.secondary(entry))}</td>
    </tr>`;
  }).join("");
  const more = entries.length > VISIBLE_ROWS
    ? `<button class="more" data-expand="${id}">${expanded ? "Show top 10" : `Show all ${entries.length}`}</button>`
    : "";
  return `<table>
    <thead><tr><th class="rank">#</th><th>Address</th><th class="num">${escape(columns.primaryLabel)}</th><th class="num secondary">${escape(columns.secondaryLabel)}</th></tr></thead>
    <tbody>${body}</tbody>
  </table>${more}`;
}

function board(id, title, scope, description, content) {
  return `<article class="board" id="${id}">
    <header><h2>${escape(title)}</h2><span class="scope">${escape(scope)}</span></header>
    <p>${escape(description)}</p>
    ${content}
  </article>`;
}

function boards(stats) {
  const mining = stats.mining;
  const windowLabel = WINDOWS.find((window) => window.id === state.window)?.label ?? state.window;
  return `<section class="boards">
    ${board("heartbeat-miners", "Heartbeat miners", windowLabel,
      "Proof of Work blocks found. Rewards are ticket payouts, inclusion fees and half of the Ledger fees.",
      rows("heartbeat-miners", mining.heartbeat_miners, {
        primaryLabel: "Blocks", secondaryLabel: "Earned",
        primary: (entry) => formatCount(entry.blocks),
        share: (entry) => share(entry.blocks, mining.heartbeats),
        secondary: (entry) => `${formatIunaShort(entry.rewards)} IUNA`,
      }))}
    ${board("ledger-winners", "Ledger winners", windowLabel,
      "Ledger lottery wins by ticket mining key. Rewards are the subsidy plus the winner's half of the fees.",
      rows("ledger-winners", mining.ledger_winners, {
        primaryLabel: "Wins", secondaryLabel: "Earned",
        primary: (entry) => formatCount(entry.blocks),
        share: (entry) => share(entry.blocks, mining.ledger_blocks),
        secondary: (entry) => `${formatIunaShort(entry.rewards)} IUNA`,
      }))}
    ${board("tickets", "Ticket weight", "Now",
      "Lottery weight of active tickets in the current Heartbeat, and expenditure still locked in tracked tickets.",
      rows("tickets", stats.tickets, {
        primaryLabel: "Weight", secondaryLabel: "Locked",
        primary: (entry) => `${formatIunaShort(entry.weight)}`,
        share: (entry) => share(entry.weight, stats.active_ticket_weight),
        secondary: (entry) => `${formatIunaShort(entry.locked)} IUNA`,
      }))}
    ${board("balances", "Largest balances", "Now",
      "Confirmed spendable outputs per address. Expenditure locked in tickets is not included.",
      rows("balances", stats.balances, {
        primaryLabel: "Balance", secondaryLabel: "Outputs",
        primary: (entry) => `${formatIunaShort(entry.amount)} IUNA`,
        share: (entry) => share(entry.amount, stats.minted),
        secondary: (entry) => formatCount(entry.outputs),
      }))}
  </section>`;
}

function statusLine() {
  const stats = state.stats;
  if (state.error) return { status: "error", label: state.error };
  if (state.loading && !state.fetchedAt) return { status: "loading", label: "Connecting…" };
  const version = stats ? ` · node v${stats.app_version}` : "";
  return { status: state.loading ? "loading" : "ok", label: `Updated ${new Date(state.fetchedAt).toLocaleTimeString()}${version}` };
}

function renderShell() {
  app.innerHTML = `
    <header class="top">
      <a class="brand" href="https://getiuna.org/"><img src="./icon.svg" alt="" width="34" height="34">iuna <b>stats</b></a>
      <span class="network" data-field="network"></span>
    </header>
    <section class="intro">
      <div>
        <h1>Leaderboard</h1>
        <p>Who is mining Heartbeats, winning the Ledger lottery and spending on tickets, as seen by one public node.</p>
      </div>
      <div class="segments" role="group" aria-label="Mining window">${WINDOWS.map((window) =>
        `<button type="button" data-window="${window.id}">${escape(window.label)}</button>`).join("")}</div>
    </section>
    <form class="source" id="node-form">
      <span class="dot" data-field="dot" aria-hidden="true"></span>
      <label>Node <input name="node" value="${escape(state.node)}" spellcheck="false" autocomplete="off"></label>
      <span class="status" data-field="status" role="status"></span>
    </form>
    <div data-field="content"></div>
    <footer><a href="https://getiuna.org/">getiuna.org</a> · <a href="https://wallet.getiuna.org/">Wallet</a> · <a href="https://github.com/iuna-labs/iuna/blob/main/docs/protocol.md">Protocol</a></footer>`;
}

const field = (name) => app.querySelector(`[data-field="${name}"]`);

// The shell stays in place, so refreshing never steals focus from the node field.
function render() {
  const stats = state.stats;
  const { status, label } = statusLine();
  field("network").textContent = stats?.network ?? "";
  field("dot").className = `dot ${status}`;
  field("status").textContent = label;
  for (const button of app.querySelectorAll("[data-window]")) {
    button.setAttribute("aria-pressed", String(button.dataset.window === state.window));
  }
  const content = field("content");
  content.innerHTML = stats
    ? `${summary(stats)}${boards(stats)}`
    : `<section class="placeholder"><p>${state.error
      ? `${escape(state.error)}. Check the node origin above; it needs a public wallet endpoint.`
      : "Loading statistics…"}</p></section>`;
  // The style-src policy forbids inline style attributes, but not CSSOM writes.
  for (const bar of content.querySelectorAll("[data-share]")) {
    bar.style.width = `${Number(bar.dataset.share) * 100}%`;
  }
}

app.addEventListener("click", async (event) => {
  const target = event.target.closest("button");
  if (!target) return;
  if (target.dataset.window && target.dataset.window !== state.window) {
    state.window = target.dataset.window;
    syncUrl();
    load();
  } else if (target.dataset.expand) {
    const id = target.dataset.expand;
    if (state.expanded.has(id)) state.expanded.delete(id); else state.expanded.add(id);
    render();
  } else if (target.dataset.copy) {
    try {
      await navigator.clipboard.writeText(target.dataset.copy);
      target.classList.add("copied");
      setTimeout(() => target.classList.remove("copied"), 1_200);
    } catch {
      // Clipboard access can be denied; the full address stays available in the tooltip.
    }
  }
});

app.addEventListener("submit", (event) => {
  if (event.target.id !== "node-form") return;
  event.preventDefault();
  const node = normalizeNode(new FormData(event.target).get("node"));
  if (!node) {
    state.error = "Use an http(s) origin such as https://stats.getiuna.org";
    state.stats = null;
    render();
    return;
  }
  state.node = node;
  const params = new URLSearchParams(location.search);
  params.set("node", node);
  history.replaceState(null, "", `${location.pathname}?${params}${location.hash}`);
  state.stats = null;
  load();
});

setInterval(() => {
  if (document.visibilityState === "visible") load({ quiet: true });
}, REFRESH_MS);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && Date.now() - (state.fetchedAt ?? 0) > REFRESH_MS) load({ quiet: true });
});

renderShell();
load();
