const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

function app() {
  const context = {
    window: {},
    localStorage: { getItem: () => null },
    setTimeout,
    clearTimeout,
    URLSearchParams,
  };
  vm.runInNewContext(readFileSync(require.resolve('../www/assets/iuna-ui.js'), 'utf8'), context);
  return context.window.iunaApp();
}

const legacyRecipient = 'iuna1q7fj9u5pqpuq0gfl4a3a7afsqfmfm22x4lgz4w8aqmggs4e33aqfqkx5kyt';
const hybridRecipient = 'iuna1pexampleaddress';

test('warns when a legacy recipient can only be paid from hybrid funds', () => {
  const ui = app();
  ui.status = { quantum_migration: { legacy_balance: 0, hybrid_balance: 5_000_000 } };

  ui.transferTo = legacyRecipient;
  assert.equal(ui.legacyRecipientNeedsHybridAddress(), true);
  assert.equal(ui.transferMaxDisabled(), true);

  ui.transferTo = hybridRecipient;
  assert.equal(ui.legacyRecipientNeedsHybridAddress(), false);

  ui.transferTo = '';
  assert.equal(ui.legacyRecipientNeedsHybridAddress(), false);
});

test('allows legacy recipients while legacy funds remain', () => {
  const ui = app();
  ui.status = { quantum_migration: { legacy_balance: 1_000, hybrid_balance: 5_000_000 } };
  ui.transferTo = legacyRecipient;

  assert.equal(ui.legacyRecipientNeedsHybridAddress(), false);
  assert.equal(ui.transferMaxDisabled(), false);
});

test('offers only legacy-address UTXOs for manual legacy selection', () => {
  const ui = app();
  ui.status = { wallet_address: 'legacy' };
  ui.walletUtxos = [
    { outpoint: { txid: 'aa', index: 0 }, address: 'iuna1phybrid', amount: 10, spendable: true },
    { outpoint: { txid: 'bb', index: 1 }, address: 'legacy', amount: 5, spendable: true },
    { outpoint: { txid: 'cc', index: 2 }, address: 'legacy', amount: 3, spendable: false },
  ];

  assert.deepEqual(ui.legacyTransferUtxos().map((utxo) => utxo.outpoint.txid), ['bb', 'cc']);
  assert.deepEqual(ui.spendableTransferUtxos().map((utxo) => utxo.outpoint.txid), ['bb']);
});

test('flags legacy address-book entries', () => {
  const ui = app();

  assert.equal(ui.isLegacyAddress(legacyRecipient), true);
  assert.equal(ui.isLegacyAddress('959beb572b586708051a76b272026ebe99ae17654ffd6518357d943fbf517026'), true);
  assert.equal(ui.isLegacyAddress('tiuna1pexample'), false);
  assert.equal(ui.isLegacyAddress(hybridRecipient), false);
});
