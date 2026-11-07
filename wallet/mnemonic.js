import { WORDS } from "./words.js";
import { normalizeSeed } from "./wallet-core.js";

export async function mnemonicFromEntropy(entropy) {
  if (!(entropy instanceof Uint8Array) || entropy.length !== 32) throw new Error("A recovery phrase requires 32 bytes of entropy");
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  const bits = [...entropy].map((byte) => byte.toString(2).padStart(8, "0")).join("") + checksum[0].toString(2).padStart(8, "0");
  return Array.from({ length: 24 }, (_, index) => WORDS[Number.parseInt(bits.slice(index * 11, index * 11 + 11), 2)]).join(" ");
}

export async function generateMnemonic() {
  return mnemonicFromEntropy(crypto.getRandomValues(new Uint8Array(32)));
}

export async function mnemonicEntropy(value) {
  const words = normalizeSeed(value).split(" ");
  if (words.length !== 24) throw new Error("The recovery phrase must contain exactly 24 words");
  const indexes = words.map((word) => WORDS.indexOf(word));
  if (indexes.some((index) => index < 0)) throw new Error("The recovery phrase contains an unknown BIP-39 word");
  const bits = indexes.map((index) => index.toString(2).padStart(11, "0")).join("");
  const entropy = Uint8Array.from(bits.slice(0, 256).match(/.{8}/g), (byte) => Number.parseInt(byte, 2));
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  if (checksum[0].toString(2).padStart(8, "0") !== bits.slice(256)) throw new Error("The recovery phrase checksum is invalid");
  return entropy;
}

export async function validateMnemonic(value) {
  await mnemonicEntropy(value);
  return normalizeSeed(value);
}
