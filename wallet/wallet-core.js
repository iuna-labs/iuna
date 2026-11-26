const encoder = new TextEncoder();
// Signatures are scoped to one network, so they cannot be replayed on a fork or relaunch.
const NETWORK_ID = "iuna-mainnet-candidate";
const GENESIS_HASH = hexBytes("00000015ff94624a51c05e7bf53e0b837baf105d9cf28d0af49955f1a8b24fdb");
const PKCS8_ED25519_PREFIX = hexBytes("302e020100300506032b657004220420");
const ADDRESS_GAP_LIMIT = 20;
// Matches the node wallet keyring. Discovery stops here, also when a node falsely reports every
// address as used.
const ADDRESS_SCAN_LIMIT = 1_000;
// Change stays below this, so the gap scan past the last used change index never reaches the scan
// limit. Matches the node wallet's WALLET_CHANGE_ADDRESS_LIMIT.
const CHANGE_ADDRESS_LIMIT = ADDRESS_SCAN_LIMIT - ADDRESS_GAP_LIMIT;
// Matches the node limit: the root plus one gap window of both branches fits in one request.
const MAX_ADDRESS_BATCH = 64;
const RECEIVE_BRANCH = 0;
const CHANGE_BRANCH = 1;
// Canonical sizes, as the node wallet counts them: a signed input is its outpoint (32 + 4), public
// key (32) and length-prefixed signature (4 + 64); an output is an address and a u64 value.
const SIGNED_INPUT_BYTES = 136;
const OUTPUT_BYTES = 40;
const TRANSACTION_FIXED_BYTES = 10;
// The node's MAX_LEDGER_TRANSACTION_BYTES: a Ledger block minus its header reserve, the
// transaction count and the transaction's length prefix. A larger one could never be mined.
export const MAX_LEDGER_TRANSACTION_BYTES = 512 * 1024 - 512 - 8;
// Like the node wallet, allow for change, so sending exactly the maximum selects the same inputs.
export const MAX_TRANSFER_INPUTS = Math.floor(
  (MAX_LEDGER_TRANSACTION_BYTES - TRANSACTION_FIXED_BYTES - 2 * OUTPUT_BYTES) / SIGNED_INPUT_BYTES,
);

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
  if (location?.hostname === "wallet.getiuna.org") return location.origin;
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

export async function fetchWalletActivity(endpoint, publicKey, addresses, beforeHeight = null) {
  const body = { public_key: publicKey, addresses };
  if (beforeHeight != null) body.before_height = beforeHeight;
  return requestJson(`${endpoint}/api/public/wallet-activity`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function discoverWalletView(endpoint, wallet) {
  const branches = [RECEIVE_BRANCH, CHANGE_BRANCH].map((branch) => ({ branch, scanned: 0, highestUsed: -1 }));
  const entries = [];
  const pending = new Set();
  let first = null;
  let request = [wallet];
  for (;;) {
    for (const state of branches) {
      // A wallet that used change past the change limit, before there was one, still loads: its
      // gap window is cut off at the scan limit, where the node wallet stops as well.
      while (state.scanned < state.highestUsed + 1 + ADDRESS_GAP_LIMIT
        && state.scanned < ADDRESS_SCAN_LIMIT && request.length < MAX_ADDRESS_BATCH) {
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
  const activityAddresses = entries.map(({ key }) => key.addressHex);
  const emptyChange = emptyChangeIndexes(entries);
  const activity = await fetchWalletActivity(endpoint, wallet.publicKeyHex, activityAddresses);
  if (activity.tip_hash !== first.tip_hash) return null;
  return {
    network: first.network,
    heartbeat_height: first.heartbeat_height,
    tip_timestamp: first.tip_timestamp,
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
    change_index: changeIndexFor(branches[CHANGE_BRANCH].highestUsed, emptyChange),
    empty_change_indexes: emptyChange,
    activity_addresses: activityAddresses,
    activity,
  };
}

// The scanned change indexes below the limit that hold nothing, in ascending order.
function emptyChangeIndexes(entries) {
  return entries
    .filter(({ key, summary }) => key.branch === CHANGE_BRANCH && key.index < CHANGE_ADDRESS_LIMIT
      && Number(summary.balance) === 0 && Number(summary.utxo_count) === 0)
    .map(({ key }) => key.index)
    .sort((a, b) => a - b);
}

// The next fresh change index or, once every one below the limit was used, the lowest change
// address holding nothing. Reuse only costs privacy; a fresh index past the limit would hide
// change from a restored wallet.
function changeIndexFor(highestUsed, emptyIndexes) {
  if (highestUsed + 1 < CHANGE_ADDRESS_LIMIT) return highestUsed + 1;
  return emptyIndexes.length ? emptyIndexes[0] : 0;
}

// The change index for a second send before the next refresh: the next scanned index that holds
// nothing, so a reused address never already holds funds. Past the last one it wraps around.
export function nextChangeIndex(index, emptyIndexes = []) {
  const current = index || 0;
  const candidates = emptyIndexes.filter((candidate) => candidate !== current);
  if (candidates.length) return candidates.find((candidate) => candidate > current) ?? candidates[0];
  const next = current + 1;
  return next < CHANGE_ADDRESS_LIMIT ? next : 0;
}

// A reward has no transaction of its own: the node lists the fees and payouts it collects.
export async function fetchBlockRewards(endpoint, blockHash) {
  if (!/^[0-9a-f]{64}$/.test(blockHash)) throw new Error("Unknown block");
  return requestJson(`${endpoint}/api/public/rewards/${blockHash}`);
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

export function transactionSize(inputs, outputs) {
  return TRANSACTION_FIXED_BYTES + inputs * SIGNED_INPUT_BYTES + outputs * OUTPUT_BYTES;
}

// Node relay policy: one unit per started 1,000 canonical bytes.
export function minimumRelayFee(size) {
  return Math.ceil(size / 1_000);
}

function spendableByValue(utxos) {
  if (!Array.isArray(utxos)) throw new Error("Wallet outputs are unavailable; reconnect to a node first");
  // Largest first, as the node wallet spends: the fewest inputs keep the transaction small and
  // its fee low.
  return utxos
    .filter((utxo) => !utxo.reserved)
    .sort((left, right) => right.value - left.value
      || left.transaction_id.localeCompare(right.transaction_id)
      || left.output_index - right.output_index);
}

function selectInputs(available, required) {
  const selected = [];
  let total = 0n;
  for (const utxo of available) {
    if (total >= required) break;
    if (selected.length === MAX_TRANSFER_INPUTS) {
      throw new Error(`This transfer needs more than ${MAX_TRANSFER_INPUTS} outputs, the most one transaction can spend. `
        + `The largest ${MAX_TRANSFER_INPUTS} hold ${formatIuna(total)} IUNA: send less at a time.`);
    }
    selected.push(utxo);
    total += BigInt(utxo.value);
  }
  if (total < required) throw new Error("Insufficient spendable balance");
  return { selected, total };
}

// The inputs a transfer spends and the fee it pays: at least `fee`, raised to the relay minimum
// for its size. A higher fee can need another input, so this repeats until the fee covers it.
export function planTransfer({ utxos, amount, fee }) {
  if (amount <= 0 || fee < 0) throw new Error("Amount must be positive and fee cannot be negative");
  const available = spendableByValue(utxos);
  let paid = fee;
  for (;;) {
    const required = BigInt(amount) + BigInt(paid);
    const { selected, total } = selectInputs(available, required);
    const outputs = total > required ? 2 : 1;
    const minimum = minimumRelayFee(transactionSize(selected.length, outputs));
    if (paid >= minimum) return { selected, total, fee: paid, raised: paid > fee };
    paid = minimum;
  }
}

// The most one transaction can send: its largest spendable outputs, minus a fee of at least
// `fee` that covers the relay minimum for spending all of them.
export function maximumTransfer(utxos, fee) {
  const available = spendableByValue(utxos);
  const selected = available.slice(0, MAX_TRANSFER_INPUTS);
  const value = selected.reduce((sum, utxo) => sum + Number(utxo.value), 0);
  const paid = Math.max(fee, selected.length ? minimumRelayFee(transactionSize(selected.length, 1)) : 0);
  if (value - paid <= 0) throw new Error("The spendable balance does not cover the fee");
  return { amount: value - paid, fee: paid, inputs: selected.length, limited: selected.length < available.length };
}

export async function buildSignedTransaction({ wallet, utxos, recipient, amount, fee, changeIndex = 0 }) {
  const recipientAddress = hexBytes(recipient);
  if (recipientAddress.length !== 32) throw new Error("Recipient must be a 64-character address");
  const plan = planTransfer({ utxos, amount, fee });
  // The fee was confirmed by the user; raising it here would sign something they never saw.
  if (plan.raised) throw new Error(`The fee is below the relay minimum of ${formatIuna(plan.fee)} IUNA for this transaction`);
  const { selected, total } = plan;
  const required = BigInt(amount) + BigInt(fee);
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
  const digests = signingDigests(transaction, selected.map((utxo) => utxo.value));
  for (let index = 0; index < transaction.inputs.length; index += 1) {
    const key = selected[index].walletKey || wallet;
    transaction.inputs[index].signature = [...new Uint8Array(await crypto.subtle.sign("Ed25519", key.privateKey, digests(index)))];
  }
  return transaction;
}

// `inputValues` are signed too: a node reporting a wrong value yields an invalid signature instead
// of change that silently pays the difference as a fee.
export async function signingDigest(transaction, inputIndex, inputValues) {
  return signingDigests(transaction, inputValues)(inputIndex);
}

// Each input signs taggedHash(scope || unsigned transaction || input values || input index). All
// but the index is shared, so it is hashed once and each digest only finishes from that state:
// hashing the whole transaction per input made signing quadratic in its number of inputs.
export function signingDigests(transaction, inputValues) {
  const domain = encoder.encode("IUNA-TX-SIGN-v1");
  const shared = concat(sized(encoder.encode(NETWORK_ID)), GENESIS_HASH, canonicalTransaction(transaction, false), ...inputValues.map(u64));
  const prefix = new Sha256().update(concat(u64(domain.length), domain, u64(shared.length + 4), shared));
  return (inputIndex) => prefix.clone().update(u32(inputIndex)).digest();
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

const SHA256_K = Uint32Array.from([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

// WebCrypto cannot hash incrementally or copy a hash state, which signingDigests needs. This is
// plain FIPS 180-4 SHA-256; tests compare it with WebCrypto.
export class Sha256 {
  constructor() {
    this.state = Uint32Array.from([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
    this.block = new Uint8Array(64);
    this.used = 0;
    this.length = 0;
    this.words = new Uint32Array(64);
  }

  clone() {
    const copy = new Sha256();
    copy.state.set(this.state);
    copy.block.set(this.block);
    copy.used = this.used;
    copy.length = this.length;
    return copy;
  }

  update(bytes) {
    this.length += bytes.length;
    let offset = 0;
    while (offset < bytes.length) {
      const take = Math.min(64 - this.used, bytes.length - offset);
      this.block.set(bytes.subarray(offset, offset + take), this.used);
      this.used += take;
      offset += take;
      if (this.used === 64) {
        this.compress();
        this.used = 0;
      }
    }
    return this;
  }

  digest() {
    const bits = this.length * 8;
    this.update(new Uint8Array([0x80]));
    if (this.used > 56) this.update(new Uint8Array(64 - this.used));
    this.update(new Uint8Array(56 - this.used));
    const length = new Uint8Array(8);
    new DataView(length.buffer).setUint32(0, Math.floor(bits / 0x1_0000_0000));
    new DataView(length.buffer).setUint32(4, bits >>> 0);
    this.update(length);
    const out = new Uint8Array(32);
    const view = new DataView(out.buffer);
    this.state.forEach((word, index) => view.setUint32(index * 4, word));
    return out;
  }

  compress() {
    const w = this.words;
    const view = new DataView(this.block.buffer);
    for (let i = 0; i < 16; i += 1) w[i] = view.getUint32(i * 4);
    for (let i = 16; i < 64; i += 1) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = this.state;
    for (let i = 0; i < 64; i += 1) {
      const t1 = (h + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + SHA256_K[i] + w[i]) >>> 0;
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) >>> 0;
      h = g; g = f; f = e; e = (d + t1) >>> 0;
      d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    const state = this.state;
    state[0] += a; state[1] += b; state[2] += c; state[3] += d;
    state[4] += e; state[5] += f; state[6] += g; state[7] += h;
  }
}

function rotr(value, bits) { return (value >>> bits) | (value << (32 - bits)); }

function u16(value) { const out = new Uint8Array(2); new DataView(out.buffer).setUint16(0, Number(value)); return out; }
function u32(value) { const out = new Uint8Array(4); new DataView(out.buffer).setUint32(0, Number(value)); return out; }
function u64(value) { const out = new Uint8Array(8); new DataView(out.buffer).setBigUint64(0, BigInt(value)); return out; }
function sized(bytes) { return concat(u32(bytes.length), bytes); }
function concat(...values) { const out = new Uint8Array(values.reduce((sum, value) => sum + value.length, 0)); let offset = 0; for (const value of values) { out.set(value, offset); offset += value.length; } return out; }
function base64url(bytes) { return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""); }
function fromBase64url(value) { const normalized = value.replace(/-/g, "+").replace(/_/g, "/"); return Uint8Array.from(atob(normalized.padEnd(Math.ceil(normalized.length / 4) * 4, "=")), (character) => character.charCodeAt(0)); }
