import { WORDS } from "./words.js";
import { normalizeSeed } from "./wallet-core.js";

export async function generateMnemonic() {
  const entropy = crypto.getRandomValues(new Uint8Array(32));
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  const bits = [...entropy].map((byte) => byte.toString(2).padStart(8, "0")).join("")
    + checksum[0].toString(2).padStart(8, "0");
  return Array.from({ length: 24 }, (_, index) => WORDS[Number.parseInt(bits.slice(index * 11, index * 11 + 11), 2)]).join(" ");
}

export async function validateMnemonic(value) {
  const words = normalizeSeed(value).split(" ");
  if (words.length !== 24) throw new Error("Een seed phrase moet precies 24 woorden bevatten");
  const indexes = words.map((word) => WORDS.indexOf(word));
  if (indexes.some((index) => index < 0)) throw new Error("De seed phrase bevat een onbekend BIP-39 woord");
  const bits = indexes.map((index) => index.toString(2).padStart(11, "0")).join("");
  const entropyBits = bits.slice(0, 256);
  const checksumBits = bits.slice(256);
  const entropy = Uint8Array.from(entropyBits.match(/.{8}/g), (byte) => Number.parseInt(byte, 2));
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  if (checksum[0].toString(2).padStart(8, "0") !== checksumBits) throw new Error("De checksum van de seed phrase klopt niet");
  return normalizeSeed(value);
}
