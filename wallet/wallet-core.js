const encoder = new TextEncoder();

export const API_BASE = "https://iuna.jhx.app/v1";
export const STORAGE_KEY = "iuna.wallet.v1";
export const MICRO_IUNA = 1_000_000n;

export function bytesToHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function hexToBytes(hex) {
  if (!/^[0-9a-f]*$/.test(hex) || hex.length % 2) throw new Error("Ongeldige hexwaarde");
  return Uint8Array.from(hex.match(/.{2}/g) || [], (pair) => Number.parseInt(pair, 16));
}

function base64url(bytes) {
  let binary = "";
  bytes.forEach((byte) => { binary += String.fromCharCode(byte); });
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function fromBase64url(value) {
  const normalized = value.replaceAll("-", "+").replaceAll("_", "/");
  const binary = atob(normalized + "=".repeat((4 - normalized.length % 4) % 4));
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

export async function walletFromSeed(seedPhrase) {
  const normalized = normalizeSeed(seedPhrase);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(`iuna-wallet-seed:${normalized}`)));
  const pkcs8Prefix = hexToBytes("302e020100300506032b657004220420");
  const pkcs8 = new Uint8Array(pkcs8Prefix.length + digest.length);
  pkcs8.set(pkcs8Prefix);
  pkcs8.set(digest, pkcs8Prefix.length);
  let privateKey;
  try {
    privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, true, ["sign"]);
  } catch {
    throw new Error("Deze browser ondersteunt geen veilige Ed25519-sleutels. Gebruik een recente Safari, Chrome, Edge of Firefox.");
  }
  const jwk = await crypto.subtle.exportKey("jwk", privateKey);
  const publicKey = fromBase64url(jwk.x);
  return { seedPhrase: normalized, privateKey, publicKey, publicKeyHex: bytesToHex(publicKey) };
}

export function normalizeSeed(seed) {
  return seed.trim().toLowerCase().split(/\s+/).join(" ");
}

export async function encryptWallet(seedPhrase, password, publicKeyHex) {
  if (password.length < 10) throw new Error("Kies een wachtwoord van minimaal 10 tekens");
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const material = await crypto.subtle.importKey("raw", encoder.encode(password), "PBKDF2", false, ["deriveKey"]);
  const key = await crypto.subtle.deriveKey(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations: 310_000 },
    material,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt"],
  );
  const plaintext = encoder.encode(JSON.stringify({ seed: normalizeSeed(seedPhrase) }));
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv, additionalData: encoder.encode(publicKeyHex) },
    key,
    plaintext,
  ));
  return {
    version: 1,
    address: publicKeyHex,
    kdf: "PBKDF2-SHA256",
    iterations: 310_000,
    cipher: "AES-256-GCM",
    salt: base64url(salt),
    iv: base64url(iv),
    ciphertext: base64url(ciphertext),
  };
}

export async function decryptWallet(record, password) {
  try {
    const material = await crypto.subtle.importKey("raw", encoder.encode(password), "PBKDF2", false, ["deriveKey"]);
    const key = await crypto.subtle.deriveKey(
      { name: "PBKDF2", hash: "SHA-256", salt: fromBase64url(record.salt), iterations: record.iterations },
      material,
      { name: "AES-GCM", length: 256 },
      false,
      ["decrypt"],
    );
    const plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: fromBase64url(record.iv), additionalData: encoder.encode(record.address) },
      key,
      fromBase64url(record.ciphertext),
    );
    const wallet = await walletFromSeed(JSON.parse(new TextDecoder().decode(plaintext)).seed);
    if (wallet.publicKeyHex !== record.address) throw new Error("Adrescontrole mislukt");
    return wallet;
  } catch {
    throw new Error("Onjuist wachtwoord of beschadigde wallet");
  }
}

const CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32M = 0x2bc830a3;

function polymod(values) {
  const generators = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
  let checksum = 1;
  for (const value of values) {
    const top = checksum >>> 25;
    checksum = ((checksum & 0x1ffffff) << 5) ^ value;
    for (let index = 0; index < 5; index += 1) if ((top >>> index) & 1) checksum ^= generators[index];
    checksum >>>= 0;
  }
  return checksum >>> 0;
}

function hrpExpand(hrp) {
  return [...hrp].map((c) => c.charCodeAt(0) >>> 5).concat(0, [...hrp].map((c) => c.charCodeAt(0) & 31));
}

function convertBits(data, from, to, pad) {
  let acc = 0;
  let bits = 0;
  const result = [];
  const max = (1 << to) - 1;
  for (const value of data) {
    if (value >>> from) throw new Error("Ongeldige adresdata");
    acc = ((acc << from) | value) & ((1 << (from + to - 1)) - 1);
    bits += from;
    while (bits >= to) { bits -= to; result.push((acc >>> bits) & max); }
  }
  if (pad && bits) result.push((acc << (to - bits)) & max);
  else if (!pad && (bits >= from || ((acc << (to - bits)) & max))) throw new Error("Ongeldige adrespadding");
  return result;
}

export function encodeAddress(publicKey, networkId = "iuna-mainnet-v1") {
  const hrp = networkId.includes("testnet") || networkId.includes("e2e") ? "tiuna" : "iuna";
  const data = [0, ...convertBits(publicKey, 8, 5, true)];
  const values = [...hrpExpand(hrp), ...data, 0, 0, 0, 0, 0, 0];
  const mod = (polymod(values) ^ BECH32M) >>> 0;
  const checksum = Array.from({ length: 6 }, (_, index) => (mod >>> (5 * (5 - index))) & 31);
  return `${hrp}1${[...data, ...checksum].map((value) => CHARSET[value]).join("")}`;
}

export function decodeAddress(address, expectedHrp = "iuna") {
  const normalized = address.trim().toLowerCase();
  if (address !== address.toLowerCase() && address !== address.toUpperCase()) throw new Error("Adres gebruikt hoofdletters en kleine letters door elkaar");
  const separator = normalized.lastIndexOf("1");
  if (separator < 1 || separator + 7 > normalized.length || normalized.slice(0, separator) !== expectedHrp) throw new Error(`Dit is geen geldig ${expectedHrp}-adres`);
  const data = [...normalized.slice(separator + 1)].map((char) => {
    const value = CHARSET.indexOf(char);
    if (value < 0) throw new Error("Adres bevat een ongeldig teken");
    return value;
  });
  if (polymod([...hrpExpand(expectedHrp), ...data]) !== BECH32M) throw new Error("De adres-checksum klopt niet");
  const payload = data.slice(0, -6);
  if (payload.shift() !== 0) throw new Error("Niet-ondersteunde adresversie");
  const key = Uint8Array.from(convertBits(payload, 5, 8, false));
  if (key.length !== 32) throw new Error("Ongeldige adressleutel");
  return bytesToHex(key);
}

function pushU32(target, value) {
  const view = new DataView(new ArrayBuffer(4));
  view.setUint32(0, value, false);
  target.push(...new Uint8Array(view.buffer));
}

function pushU64(target, value) {
  const view = new DataView(new ArrayBuffer(8));
  view.setBigUint64(0, BigInt(value), false);
  target.push(...new Uint8Array(view.buffer));
}

function pushBytes(target, value) {
  pushU32(target, value.length);
  target.push(...value);
}

export function transferSigningBytes(status, inputs, outputs, fee) {
  const bytes = [...encoder.encode("IUNA-TX"), 0, Number(status.transaction_signing_format_version || 1)];
  pushBytes(bytes, encoder.encode(status.chain_id));
  pushBytes(bytes, hexToBytes(status.genesis_hash));
  bytes.push(1);
  pushU32(bytes, inputs.length);
  inputs.forEach((input) => {
    pushBytes(bytes, hexToBytes(input.outpoint.txid));
    pushU32(bytes, input.outpoint.index);
    pushBytes(bytes, hexToBytes(input.owner));
  });
  pushU32(bytes, outputs.length);
  outputs.forEach((output) => {
    pushBytes(bytes, hexToBytes(output.address));
    pushU64(bytes, output.amount);
  });
  pushU64(bytes, fee);
  return Uint8Array.from(bytes);
}

function legacyTransferPayload(inputs, outputs, fee) {
  const canonicalInputs = inputs.map((input) => `${input.outpoint.txid}:${input.outpoint.index}:${input.owner}`).join("|");
  const canonicalOutputs = outputs.map((output) => `${output.address}:${output.amount}`).join("|");
  return `utxo-transfer:${canonicalInputs}:${canonicalOutputs}:${fee}`;
}

function compactLength(value) {
  let n = BigInt(value);
  let length = 1;
  while (n >= 128n) { n >>= 7n; length += 1; }
  return length;
}

function protocolIdSize(hex) { return 1 + hex.length / 2; }

export function estimateTransferSize(inputs, outputs, fee) {
  return 1 + compactLength(inputs.length)
    + inputs.reduce((sum, input) => sum + protocolIdSize(input.outpoint.txid) + compactLength(input.outpoint.index) + 33, 0)
    + compactLength(outputs.length)
    + outputs.reduce((sum, output) => sum + 33 + compactLength(output.amount), 0)
    + compactLength(fee) + 65;
}

export function selectInputs(utxos, amount, feeRate, owner, recipient) {
  const sorted = [...utxos].sort((a, b) => Number(BigInt(b.output.amount) - BigInt(a.output.amount)));
  const selected = [];
  let total = 0n;
  let fee = 0n;
  for (const utxo of sorted) {
    selected.push({ outpoint: utxo.outpoint, owner });
    total += BigInt(utxo.output.amount);
    for (let pass = 0; pass < 8; pass += 1) {
      const provisionalChange = total - amount - fee;
      const outputs = [{ address: recipient, amount }];
      if (provisionalChange > 0n) outputs.push({ address: owner, amount: provisionalChange });
      const nextFee = BigInt(estimateTransferSize(selected, outputs, fee)) * BigInt(feeRate);
      if (nextFee === fee) break;
      fee = nextFee;
    }
    if (total >= amount + fee) return { inputs: selected, total, fee };
  }
  throw new Error("Onvoldoende besteedbaar saldo voor bedrag en netwerkkosten");
}

export async function buildSignedTransfer({ wallet, status, utxos, recipientAddress, amount }) {
  const expectedHrp = status.chain_id.includes("testnet") || status.chain_id.includes("e2e") ? "tiuna" : "iuna";
  const recipient = decodeAddress(recipientAddress, expectedHrp);
  const feeRate = BigInt(status.default_fee_per_byte || 1);
  const { inputs, total, fee } = selectInputs(utxos, amount, feeRate, wallet.publicKeyHex, recipient);
  const outputs = [{ address: recipient, amount }];
  const change = total - amount - fee;
  if (change > 0n) outputs.push({ address: wallet.publicKeyHex, amount: change });
  const activation = BigInt(status.transaction_signing_v1_activation_height || 1000);
  const signingHeight = BigInt(status.height || 0) + 1n;
  const signingPayload = signingHeight >= activation
    ? transferSigningBytes(status, inputs, outputs, fee)
    : encoder.encode(legacyTransferPayload(inputs, outputs, fee));
  const signature = bytesToHex(new Uint8Array(await crypto.subtle.sign("Ed25519", wallet.privateKey, signingPayload)));
  return {
    transaction: {
      kind: "transfer",
      inputs: inputs.map((input) => ({ ...input, signature })),
      outputs: outputs.map((output) => ({ ...output, amount: Number(output.amount) })),
      fee: Number(fee),
      signature,
    },
    fee,
  };
}

export function parseIuna(value) {
  const normalized = value.trim().replace(",", ".");
  if (!/^\d+(\.\d{0,6})?$/.test(normalized)) throw new Error("Vul een geldig bedrag in (maximaal 6 decimalen)");
  const [whole, fraction = ""] = normalized.split(".");
  return BigInt(whole) * MICRO_IUNA + BigInt(fraction.padEnd(6, "0"));
}

export function formatIuna(value, maximumFractionDigits = 6) {
  const amount = BigInt(value || 0);
  const whole = amount / MICRO_IUNA;
  const fraction = (amount % MICRO_IUNA).toString().padStart(6, "0").slice(0, maximumFractionDigits).replace(/0+$/, "");
  return `${whole.toLocaleString("nl-NL")}${fraction ? `,${fraction}` : ""}`;
}

export async function api(path, options = {}) {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), options.timeoutMs || 8_000);
  const { timeoutMs: _timeoutMs, signal: callerSignal, ...requestOptions } = options;
  if (callerSignal) callerSignal.addEventListener("abort", () => controller.abort(), { once: true });
  let response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...requestOptions,
      signal: controller.signal,
      headers: {
        ...(requestOptions.method && requestOptions.method !== "GET" ? { "content-type": "application/json" } : {}),
        ...(requestOptions.headers || {}),
      },
    });
  } catch (error) {
    if (error.name === "AbortError") throw new Error(`Geen antwoord van iuna na ${(_timeoutMs || 8_000) / 1000} seconden`);
    throw new Error("Kan geen verbinding maken met het iuna-endpoint");
  } finally {
    window.clearTimeout(timeout);
  }
  let body;
  try { body = await response.json(); } catch { body = {}; }
  if (!response.ok) throw new Error(body.error || `Endpoint gaf status ${response.status}`);
  return body;
}
