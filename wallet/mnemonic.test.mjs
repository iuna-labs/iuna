import assert from "node:assert/strict";
import test from "node:test";

import { DICE_ROLL_COUNT, generateMnemonic, generateMnemonicFromDice, validateMnemonic } from "./mnemonic.js";

test("system entropy produces a valid 24-word mnemonic", async () => {
  const mnemonic = await generateMnemonic();
  assert.equal(mnemonic.split(" ").length, 24);
  assert.equal(await validateMnemonic(mnemonic), mnemonic);
});

test("102 dice rolls deterministically produce a valid 24-word mnemonic", async () => {
  const rolls = "123456".repeat(17);
  assert.equal(rolls.length, DICE_ROLL_COUNT);
  const first = await generateMnemonicFromDice(rolls);
  const second = await generateMnemonicFromDice(rolls);
  assert.equal(first, second);
  assert.equal(first.split(" ").length, 24);
  assert.equal(await validateMnemonic(first), first);
});

test("the lowest dice value maps to the known zero-entropy BIP-39 vector", async () => {
  const expected = `${"abandon ".repeat(23)}art`;
  assert.equal(await generateMnemonicFromDice("1".repeat(DICE_ROLL_COUNT)), expected);
});

test("dice entropy rejects the wrong number of rolls", async () => {
  await assert.rejects(() => generateMnemonicFromDice("1".repeat(101)), /exactly 102/);
  await assert.rejects(() => generateMnemonicFromDice("1".repeat(103)), /exactly 102/);
});

test("dice entropy rejects values outside a six-sided die", async () => {
  await assert.rejects(() => generateMnemonicFromDice("1".repeat(101) + "7"), /numbers 1 through 6/);
});

test("dice entropy rejects the tiny biased tail", async () => {
  await assert.rejects(() => generateMnemonicFromDice("6".repeat(102)), /without bias/);
});
