import { WORDS } from "./words.js";
import { normalizeSeed } from "./wallet-core.js";

export const DICE_ROLL_COUNT = 102;

async function mnemonicFromEntropy(entropy) {
  if (!(entropy instanceof Uint8Array) || entropy.length !== 32) {
    throw new Error("A 24-word seed requires exactly 32 bytes of entropy");
  }
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  const bits = [...entropy].map((byte) => byte.toString(2).padStart(8, "0")).join("")
    + checksum[0].toString(2).padStart(8, "0");
  return Array.from({ length: 24 }, (_, index) => WORDS[Number.parseInt(bits.slice(index * 11, index * 11 + 11), 2)]).join(" ");
}

export async function generateMnemonic() {
  const entropy = crypto.getRandomValues(new Uint8Array(32));
  return mnemonicFromEntropy(entropy);
}

export async function generateMnemonicFromDice(rolls) {
  const normalized = String(rolls).replace(/[\s,-]/g, "");
  if (/[^1-6]/.test(normalized)) throw new Error("Dice rolls may only contain the numbers 1 through 6");
  if (normalized.length !== DICE_ROLL_COUNT) {
    throw new Error(`Enter exactly ${DICE_ROLL_COUNT} dice rolls`);
  }
  const sourceRange = 6n ** BigInt(DICE_ROLL_COUNT);
  const entropyRange = 1n << 256n;
  // Keep only a whole number of 256-bit ranges so modulo cannot favor any seed.
  // With 102 fair rolls the rejection chance is about 0.058%.
  const unbiasedLimit = sourceRange - sourceRange % entropyRange;
  const value = [...normalized].reduce((result, roll) => result * 6n + BigInt(Number(roll) - 1), 0n);
  if (value >= unbiasedLimit) {
    throw new Error("This exceptionally rare roll sequence cannot be used without bias. Reset and roll again");
  }
  let entropyValue = value % entropyRange;
  const entropy = new Uint8Array(32);
  for (let index = entropy.length - 1; index >= 0; index -= 1) {
    entropy[index] = Number(entropyValue & 0xffn);
    entropyValue >>= 8n;
  }
  return mnemonicFromEntropy(entropy);
}

export async function validateMnemonic(value) {
  const words = normalizeSeed(value).split(" ");
  if (words.length !== 24) throw new Error("A seed phrase must contain exactly 24 words");
  const indexes = words.map((word) => WORDS.indexOf(word));
  if (indexes.some((index) => index < 0)) throw new Error("The seed phrase contains an unknown BIP-39 word");
  const bits = indexes.map((index) => index.toString(2).padStart(11, "0")).join("");
  const entropyBits = bits.slice(0, 256);
  const checksumBits = bits.slice(256);
  const entropy = Uint8Array.from(entropyBits.match(/.{8}/g), (byte) => Number.parseInt(byte, 2));
  const checksum = new Uint8Array(await crypto.subtle.digest("SHA-256", entropy));
  if (checksum[0].toString(2).padStart(8, "0") !== checksumBits) throw new Error("The seed phrase checksum is invalid");
  return normalizeSeed(value);
}
