const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');

let core;

test.before(async () => {
  const source = readFileSync(require.resolve('../wallet/wallet-core.js'), 'utf8');
  core = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
});

test('migrates the single-wallet record into a named wallet collection', () => {
  const legacy = { address: 'ab'.repeat(32), ciphertext: 'encrypted' };
  const store = core.normalizeWalletStore(null, legacy);

  assert.equal(store.version, 2);
  assert.equal(store.wallets.length, 1);
  assert.equal(store.wallets[0].type, 'signing');
  assert.equal(store.wallets[0].record, legacy);
  assert.equal(store.activeId, store.wallets[0].id);
});

test('adds, selects, and removes multiple wallet types', () => {
  const signing = { id: 'signing', name: 'Daily', type: 'signing', publicKeyHex: '11'.repeat(32), record: { ciphertext: 'secret' } };
  const readonly = { id: 'readonly', name: 'Savings', type: 'readonly', publicKeyHex: '22'.repeat(32) };
  let store = core.upsertWallet(core.emptyWalletStore(), signing);
  store = core.upsertWallet(store, readonly);

  assert.deepEqual(store.wallets.map((wallet) => wallet.name), ['Daily', 'Savings']);
  assert.equal(store.activeId, 'readonly');
  assert.equal('record' in store.wallets[1], false);

  store = core.removeWallet(store, 'readonly');
  assert.equal(store.activeId, 'signing');
  assert.deepEqual(store.wallets.map((wallet) => wallet.id), ['signing']);
});

test('refuses to sign with a watch-only wallet', async () => {
  await assert.rejects(
    core.buildSignedTransfer({ wallet: { publicKeyHex: '22'.repeat(32) } }),
    /watch-only wallet cannot sign/,
  );
});

test('explains oversized transactions before submission', () => {
  const signature = 'aa'.repeat(64);
  const transaction = {
    kind: 'transfer',
    inputs: Array.from({ length: 205 }, (_, index) => ({
      outpoint: { txid: 'bb'.repeat(32), index },
      owner: 'cc'.repeat(32),
      signature,
    })),
    outputs: [
      { address: 'dd'.repeat(32), amount: 1 },
      { address: 'cc'.repeat(32), amount: 1 },
    ],
    fee: 1,
    signature,
  };

  assert.throws(
    () => core.ensureTransactionBodySize(transaction),
    /205 inputs, 65 KiB; maximum 64 KiB.*smaller amount.*consolidate/s,
  );
});
