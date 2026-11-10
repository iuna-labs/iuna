const encoder = new TextEncoder();
// Signatures are scoped to one network, so they cannot be replayed on a fork or relaunch.
const NETWORK_ID = "iuna-mainnet-candidate";
const GENESIS_HASH = hexBytes("00000015ff94624a51c05e7bf53e0b837baf105d9cf28d0af49955f1a8b24fdb");
const PKCS8_ED25519_PREFIX = hexBytes("302e020100300506032b657004220420");
const ADDRESS_GAP_LIMIT = 20;
// Matches the node limit: the root plus one gap window of both branches fits in one request.
const MAX_ADDRESS_BATCH = 64;
const RECEIVE_BRANCH = 0;
const CHANGE_BRANCH = 1;

export function normalizeSeed(value) {
  return String(value).trim().toLowerCase().replace(/\s+/g, " ");
}

export function apiEndpoint(location = globalThis.location) {
  const configured = globalThis.localStorage?.getItem("iunaWalletApi");
  if (configured) return configured.replace(/\/$/, "");
  return defaultApiEndpoint(location);
}

// The hosted wallet talks to its own origin (the ingress routes /api/public to a node); a local
// copy defaults to the Docker Compose bootstrap node.
export function defaultApiEndpoint(location = globalThis.location) {
  if (["wallet.getiuna.com", "wallet.getiuna.org"].includes(location?.hostname)) return location.origin;
  return "http://127.0.0.1:18662";
}

export async function walletFromEntropy(entropy) {
  if (!(entropy instanceof Uint8Array) || entropy.length !== 32) throw new Error("Wallet entropy must be 32 bytes");
  const masterEntropy = entropy.slice();
  const root = await walletKey(masterEntropy);
  return {
    ...root,
    masterEntropy,
    async keyAt(branch, index) {
      if (![RECEIVE_BRANCH, CHANGE_BRANCH].includes(branch) || !Number.isSafeInteger(index) || index < 0 || index > 0xffffffff) {
        throw new Error("Invalid wallet derivation path");
      }
      const seed = await taggedHash("IUNA-WALLET-CHILD-v1", concat(masterEntropy, new Uint8Array([branch]), u32(index)));
      return walletKey(seed, branch, index);
    },
  };
}

async function walletKey(seed, branch = null, index = null) {
  const pkcs8 = concat(PKCS8_ED25519_PREFIX, seed);
  // WebCrypto only derives the public key through an export, so a short-lived
  // extractable copy is used for that; the retained signing key is non-extractable.
  const exportable = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, true, ["sign"]);
  const jwk = await crypto.subtle.exportKey("jwk", exportable);
  if (!jwk.x) throw new Error("This browser cannot derive an Ed25519 public key");
  const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, false, ["sign"]);
  pkcs8.fill(0);
  const publicKey = fromBase64url(jwk.x);
  const address = await taggedHash("IUNA-ADDRESS-v1", publicKey);
  return { privateKey, publicKey, publicKeyHex: bytesHex(publicKey), address, addressHex: bytesHex(address), branch, index };
}

export async function encryptWallet(seedPhrase, password, publicKeyHex) {
  if (password.length < 10) throw new Error("Use a password of at least 10 characters");
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const key = await passwordKey(password, salt, ["encrypt"]);
  const plaintext = encoder.encode(JSON.stringify({ seedPhrase: normalizeSeed(seedPhrase) }));
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv, additionalData: encoder.encode(publicKeyHex) }, key, plaintext,
  ));
  return {
    version: 1,
    publicKey: publicKeyHex,
    kdf: "PBKDF2-SHA256",
    iterations: 310_000,
    cipher: "AES-256-GCM",
    salt: base64url(salt),
    iv: base64url(iv),
    ciphertext: base64url(ciphertext),
  };
}

export async function decryptWallet(record, password, entropyFromMnemonic) {
  try {
    const salt = fromBase64url(record.salt);
    const key = await passwordKey(password, salt, ["decrypt"]);
    const plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: fromBase64url(record.iv), additionalData: encoder.encode(record.publicKey) },
      key,
      fromBase64url(record.ciphertext),
    );
    const { seedPhrase } = JSON.parse(new TextDecoder().decode(plaintext));
    const wallet = await walletFromEntropy(await entropyFromMnemonic(seedPhrase));
    if (wallet.publicKeyHex !== record.publicKey) throw new Error("public key mismatch");
    return { ...wallet, seedPhrase };
  } catch {
    throw new Error("Incorrect password or damaged wallet");
  }
}

async function passwordKey(password, salt, usages) {
  const material = await crypto.subtle.importKey("raw", encoder.encode(password), "PBKDF2", false, ["deriveKey"]);
  return crypto.subtle.deriveKey(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations: 310_000 },
    material,
    { name: "AES-GCM", length: 256 },
    false,
    usages,
  );
}

async function fetchAddressView(endpoint, walletKey) {
  const path = `${endpoint}/api/public/wallet/${walletKey.addressHex}`;
  // Keep every page on one Heartbeat tip. If the chain advances while paging, restart so input
  // selection never mixes duplicate or skipped offsets from two different UTXO sets.
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const first = await requestJson(path);
    const utxos = [...first.utxos];
    let offset = first.next_utxo_offset;
    let consistent = true;
    while (offset != null) {
      const page = await requestJson(`${path}?utxo_offset=${offset}`);
      if (page.tip_hash !== first.tip_hash) {
        consistent = false;
        break;
      }
      utxos.push(...page.utxos);
      const next = page.next_utxo_offset;
      if (next != null && next <= offset) throw new Error("The node returned an invalid wallet page");
      offset = next;
    }
    if (consistent) return {
      ...first,
      utxos: utxos.map((utxo) => ({ ...utxo, walletKey })),
      utxos_truncated: false,
      next_utxo_offset: null,
    };
  }
  throw new Error("The wallet changed while outputs were loading; try again");
}

export async function fetchWalletView(endpoint, wallet) {
  // A string remains useful for endpoint diagnostics and older embedding code.
  if (typeof wallet === "string") {
    return fetchAddressView(endpoint, { addressHex: wallet });
  }
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const view = await discoverWalletView(endpoint, wallet);
    if (view) return view;
  }
  throw new Error("The wallet changed while addresses were loading; try again");
}

// One summary request per gap window: full output pages are fetched only for addresses that
// still hold outputs, so an empty deterministic window costs a single round trip.
async function fetchAddressSummaries(endpoint, keys) {
  const batch = await requestJson(`${endpoint}/api/public/wallet-addresses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addresses: keys.map((key) => key.addressHex) }),
  });
  if (!Array.isArray(batch.addresses) || batch.addresses.length !== keys.length
    || batch.addresses.some((summary, index) => summary.address !== keys[index].addressHex)) {
    throw new Error("The node returned an invalid address summary");
  }
  return batch;
}

async function discoverWalletView(endpoint, wallet) {
  const branches = [RECEIVE_BRANCH, CHANGE_BRANCH].map((branch) => ({ branch, scanned: 0, highestUsed: -1 }));
  const entries = [];
  const pending = new Set();
  let first = null;
  let request = [wallet];
  for (;;) {
    for (const state of branches) {
      while (state.scanned < state.highestUsed + 1 + ADDRESS_GAP_LIMIT && request.length < MAX_ADDRESS_BATCH) {
        request.push(await wallet.keyAt(state.branch, state.scanned));
        state.scanned += 1;
      }
    }
    if (!request.length) break;
    const batch = await fetchAddressSummaries(endpoint, request);
    if (first && batch.tip_hash !== first.tip_hash) return null;
    first ||= batch;
    for (const id of batch.pending_transaction_ids || []) pending.add(id);
    request.forEach((key, index) => {
      const summary = batch.addresses[index];
      entries.push({ key, summary });
      if (summary.used && key.branch != null) {
        branches[key.branch].highestUsed = Math.max(branches[key.branch].highestUsed, key.index);
      }
    });
    request = [];
  }
  const views = await Promise.all(entries
    .filter(({ summary }) => Number(summary.utxo_count) > 0)
    .map(({ key }) => fetchAddressView(endpoint, key)));
  if (views.some((item) => item.tip_hash !== first.tip_hash)) return null;
  for (const id of views.flatMap((item) => item.pending_transaction_ids || [])) pending.add(id);
  const receiveIndex = branches[RECEIVE_BRANCH].highestUsed + 1;
  const receiveKey = await wallet.keyAt(RECEIVE_BRANCH, receiveIndex);
  return {
    network: first.network,
    heartbeat_height: first.heartbeat_height,
    tip_hash: first.tip_hash,
    ledger_blocks: first.ledger_blocks,
    balance: entries.reduce((sum, { summary }) => sum + Number(summary.balance), 0),
    spendable_balance: entries.reduce((sum, { summary }) => sum + Number(summary.spendable_balance), 0),
    utxos: views.flatMap((item) => item.utxos),
    pending_transaction_ids: [...pending],
    utxos_truncated: false,
    next_utxo_offset: null,
    receive_address: receiveKey.addressHex,
    receive_index: receiveIndex,
    change_index: branches[CHANGE_BRANCH].highestUsed + 1,
  };
}

export async function submitTransaction(endpoint, transaction) {
  return requestJson(`${endpoint}/api/public/transactions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(transaction),
  });
}

async function requestJson(url, options) {
  const response = await fetch(url, { cache: "no-store", ...options });
  const isJson = (response.headers.get("content-type") || "").includes("application/json");
  const body = isJson ? await response.json().catch(() => null) : null;
  if (!response.ok) throw new Error(body?.error || `Request failed (${response.status})`);
  if (!body || typeof body !== "object") throw new Error("The node returned an unexpected response; check the wallet endpoint");
  return body;
}

// The outputs a signed transaction spends, as `transaction_id:output_index` keys.
export function spentOutpoints(transaction) {
  return transaction.inputs.map(({ previous_output: output }) => {
    const id = output.transaction_id.map((byte) => byte.toString(16).padStart(2, "0")).join("");
    return `${id}:${output.output_index}`;
  });
}

// A transaction spends all inputs at once, so one unspent input proves it was not mined: it left
// the mempool without confirming and the coins never moved.
export function sentStatus(item, view) {
  if (!view?.tip_hash || (view.pending_transaction_ids || []).includes(item.id)) return "pending";
  const unspent = new Set((view.utxos || []).map((utxo) => `${utxo.transaction_id}:${utxo.output_index}`));
  return (item.inputs || []).some((input) => unspent.has(input)) ? "dropped" : "sent";
}

export async function buildSignedTransaction({ wallet, utxos, recipient, amount, fee, changeIndex = 0 }) {
  const recipientAddress = hexBytes(recipient);
  if (recipientAddress.length !== 32) throw new Error("Recipient must be a 64-character address");
  const required = BigInt(amount) + BigInt(fee);
  if (amount <= 0 || fee < 0) throw new Error("Amount must be positive and fee cannot be negative");
  if (!Array.isArray(utxos)) throw new Error("Wallet outputs are unavailable; reconnect to a node first");
  const available = utxos
    .filter((utxo) => !utxo.reserved)
    .sort((left, right) => left.transaction_id.localeCompare(right.transaction_id) || left.output_index - right.output_index);
  const selected = [];
  let total = 0n;
  for (const utxo of available) {
    selected.push(utxo);
    total += BigInt(utxo.value);
    if (total >= required) break;
  }
  if (total < required) throw new Error("Insufficient spendable balance");
  const transaction = {
    version: 3,
    inputs: selected.map((utxo) => ({
      previous_output: {
        transaction_id: [...hexBytes(utxo.transaction_id)],
        output_index: utxo.output_index,
      },
      public_key: [...(utxo.walletKey || wallet).publicKey],
      signature: [],
    })),
    outputs: [{ owner: [...recipientAddress], value: Number(amount) }],
  };
  const change = total - required;
  if (change > 0n) {
    const changeKey = await wallet.keyAt(CHANGE_BRANCH, changeIndex);
    transaction.outputs.push({ owner: [...changeKey.address], value: Number(change) });
  }
  for (let index = 0; index < transaction.inputs.length; index += 1) {
    const digest = await signingDigest(transaction, index, selected.map((utxo) => utxo.value));
    const key = selected[index].walletKey || wallet;
    transaction.inputs[index].signature = [...new Uint8Array(await crypto.subtle.sign("Ed25519", key.privateKey, digest))];
  }
  return transaction;
}

// `inputValues` are signed too: a node reporting a wrong value yields an invalid signature instead
// of change that silently pays the difference as a fee.
export async function signingDigest(transaction, inputIndex, inputValues) {
  const unsigned = canonicalTransaction(transaction, false);
  const scope = concat(sized(encoder.encode(NETWORK_ID)), GENESIS_HASH);
  return taggedHash("IUNA-TX-SIGN-v1", concat(scope, unsigned, ...inputValues.map(u64), u32(inputIndex)));
}

export function canonicalTransaction(transaction, includeSignatures = true) {
  const chunks = [u16(transaction.version), u32(transaction.inputs.length)];
  for (const input of transaction.inputs) {
    chunks.push(new Uint8Array(input.previous_output.transaction_id), u32(input.previous_output.output_index), new Uint8Array(input.public_key));
    if (includeSignatures) chunks.push(sized(new Uint8Array(input.signature)));
  }
  chunks.push(u32(transaction.outputs.length));
  for (const output of transaction.outputs) chunks.push(new Uint8Array(output.owner), u64(output.value));
  return concat(...chunks);
}

async function taggedHash(domain, bytes) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", concat(u64(encoder.encode(domain).length), encoder.encode(domain), u64(bytes.length), bytes)));
}

export function parseIuna(value) {
  const match = String(value).trim().match(/^(\d+)(?:\.(\d{1,8}))?$/);
  if (!match) throw new Error("Use an IUNA amount with at most 8 decimals");
  const amount = BigInt(match[1]) * 100_000_000n + BigInt((match[2] || "").padEnd(8, "0"));
  if (amount > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("Amount is too large");
  return Number(amount);
}

export function formatIuna(value) {
  const amount = BigInt(value || 0);
  const whole = amount / 100_000_000n;
  const fraction = (amount % 100_000_000n).toString().padStart(8, "0").replace(/0+$/, "");
  return fraction ? `${whole}.${fraction}` : whole.toString();
}

export function hexBytes(value) {
  const normalized = String(value).trim().toLowerCase();
  if (!/^(?:[0-9a-f]{2})+$/.test(normalized)) throw new Error("Expected hexadecimal bytes");
  return Uint8Array.from(normalized.match(/../g), (byte) => Number.parseInt(byte, 16));
}

export function bytesHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function u16(value) { const out = new Uint8Array(2); new DataView(out.buffer).setUint16(0, Number(value)); return out; }
function u32(value) { const out = new Uint8Array(4); new DataView(out.buffer).setUint32(0, Number(value)); return out; }
function u64(value) { const out = new Uint8Array(8); new DataView(out.buffer).setBigUint64(0, BigInt(value)); return out; }
function sized(bytes) { return concat(u32(bytes.length), bytes); }
function concat(...values) { const out = new Uint8Array(values.reduce((sum, value) => sum + value.length, 0)); let offset = 0; for (const value of values) { out.set(value, offset); offset += value.length; } return out; }
function base64url(bytes) { return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""); }
function fromBase64url(value) { const normalized = value.replace(/-/g, "+").replace(/_/g, "/"); return Uint8Array.from(atob(normalized.padEnd(Math.ceil(normalized.length / 4) * 4, "=")), (character) => character.charCodeAt(0)); }
