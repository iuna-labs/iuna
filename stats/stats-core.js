// Pure helpers for the statistics page, kept free of DOM access so they can be tested in Node.

export const UNITS_PER_IUNA = 100_000_000n;

export const WINDOWS = [
  { id: "hour", label: "1 hour" },
  { id: "day", label: "24 hours" },
  { id: "week", label: "7 days" },
  { id: "all", label: "All time" },
];

// The hosted page talks to its own origin (the ingress routes /api/public to a known node); a
// local copy defaults to the Docker Compose bootstrap node.
export function defaultNode(location = globalThis.location) {
  if (location?.hostname === "stats.getiuna.org") return location.origin;
  return "http://127.0.0.1:18662";
}

/// A node given as `?node=` must be a bare http(s) origin. Anything else falls back to the default.
export function normalizeNode(value) {
  const trimmed = String(value ?? "").trim().replace(/\/+$/, "");
  if (!trimmed) return null;
  const withScheme = /^[a-z]+:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  let url;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  if (url.username || url.password || url.search || url.hash || url.pathname !== "/") return null;
  return url.origin;
}

export function resolveNode(location = globalThis.location) {
  const requested = new URLSearchParams(location?.search ?? "").get("node");
  return normalizeNode(requested) ?? defaultNode(location);
}

export function resolveWindow(location = globalThis.location) {
  const requested = new URLSearchParams(location?.search ?? "").get("window");
  return WINDOWS.some((window) => window.id === requested) ? requested : "day";
}

export function statsUrl(node, window) {
  return `${node}/api/public/stats?window=${encodeURIComponent(window)}`;
}

/// Exact decimal IUNA, without trailing zeros.
export function formatIuna(value) {
  const amount = BigInt(value || 0);
  const whole = amount / UNITS_PER_IUNA;
  const fraction = (amount % UNITS_PER_IUNA).toString().padStart(8, "0").replace(/0+$/, "");
  return `${groupDigits(whole.toString())}${fraction ? `.${fraction}` : ""}`;
}

/// IUNA rounded for tables: at most two decimals above 1 IUNA, more below it.
export function formatIunaShort(value) {
  const iuna = Number(value || 0) / 1e8;
  if (iuna === 0) return "0";
  const digits = iuna >= 1 ? 2 : iuna >= 0.01 ? 4 : 8;
  return iuna.toLocaleString("en-US", { maximumFractionDigits: digits });
}

export function formatCount(value) {
  return Number(value || 0).toLocaleString("en-US");
}

export function share(part, whole) {
  const numerator = Number(part || 0);
  const denominator = Number(whole || 0);
  if (denominator <= 0 || numerator <= 0) return 0;
  return Math.min(1, numerator / denominator);
}

export function formatShare(fraction) {
  if (fraction <= 0) return "0%";
  if (fraction < 0.001) return "<0.1%";
  return `${(fraction * 100).toLocaleString("en-US", { maximumFractionDigits: 1 })}%`;
}

export function shortAddress(address) {
  const value = String(address ?? "");
  return value.length > 16 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value;
}

export function formatAge(seconds) {
  if (!Number.isFinite(seconds)) return "unknown";
  const value = Math.max(0, Math.round(seconds));
  if (value < 60) return `${value}s ago`;
  if (value < 3_600) return `${Math.floor(value / 60)}m ago`;
  if (value < 86_400) return `${Math.floor(value / 3_600)}h ago`;
  return `${Math.floor(value / 86_400)}d ago`;
}

/// Mean spacing between the window's Heartbeats, in seconds.
export function averageInterval(mining) {
  const { heartbeats, from_timestamp: from, to_timestamp: to } = mining ?? {};
  if (!heartbeats || heartbeats < 2 || from == null || to == null) return null;
  return (to - from) / (heartbeats - 1);
}

export function formatDuration(seconds) {
  if (seconds == null || !Number.isFinite(seconds)) return "–";
  if (seconds < 120) return `${Math.round(seconds)}s`;
  return `${(seconds / 60).toLocaleString("en-US", { maximumFractionDigits: 1 })}m`;
}

export function groupDigits(digits) {
  return digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}
