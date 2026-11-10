import { readFileSync, writeFileSync } from "node:fs";

const source = readFileSync(new URL("../wallet/wallet-core.js", import.meta.url), "utf8");
const core = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const seed = new Uint8Array(32);
const wallet = await core.walletFromEntropy(seed);
const receive0 = await wallet.keyAt(0, 0);
const change0 = await wallet.keyAt(1, 0);
const transaction = await core.buildSignedTransaction({
  wallet,
  utxos: [
    { transaction_id: "11".repeat(32), output_index: 3, value: 1000, reserved: false },
    { transaction_id: "33".repeat(32), output_index: 0, value: 500, reserved: false },
  ],
  recipient: "22".repeat(32),
  amount: 1200,
  fee: 10,
  changeIndex: 0,
});
const inputValues = [1000, 500];
const vector = {
  seed_hex: core.bytesHex(seed),
  public_key_hex: wallet.publicKeyHex,
  address_hex: wallet.addressHex,
  receive_0_address_hex: receive0.addressHex,
  change_0_address_hex: change0.addressHex,
  genesis_hash: "00000015ff94624a51c05e7bf53e0b837baf105d9cf28d0af49955f1a8b24fdb",
  signing_digests: await Promise.all(
    transaction.inputs.map(async (_, index) => core.bytesHex(await core.signingDigest(transaction, index, inputValues))),
  ),
  transaction,
  input_values: inputValues,
};
writeFileSync(new URL("../tests/vectors/browser_wallet_transaction.json", import.meta.url), `${JSON.stringify(vector, null, 2)}\n`);
