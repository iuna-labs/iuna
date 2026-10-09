const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

function app() {
  const context = { window: {}, localStorage: { getItem: () => null }, setTimeout, clearTimeout, URLSearchParams };
  vm.runInNewContext(readFileSync(require.resolve('../www/assets/iuna-ui.js'), 'utf8'), context);
  const app = context.window.iunaApp();
  app.status = { wallet_address: 'wallet', wallet_locked: false };
  app.refresh = async () => {};
  app.optimizePlan = { address: 'wallet', before: 5, after: 3, fee: 200, rate: 1, mergeRoots: false,
    batches: [{ fee: 100, utxos: ['one', 'two'] }, { fee: 100, utxos: ['three', 'four'] }] };
  return app;
}

test('does not submit before approval and queues each batch after confirmation', async () => {
  const ui = app();
  ui.optimizeOpen = true;
  const events = [];
  ui.submitForm = async (_path, fields) => {
    events.push(`submit:${fields.utxos}`);
    assert.equal(fields.max_fee, 100);
    assert.equal(fields.address, 'wallet');
    return { signature: `tx${events.length}` };
  };
  ui.showFlash = () => {};
  assert.equal(events.length, 0);
  await ui.runOptimization();
  assert.equal(events.length, 2);
  assert.equal(ui.optimizePlan, null);
  assert.equal(ui.optimizeOpen, false);
  assert.match(ui.optimizeMessage, /submitted/);
});

test('runs all approved batches as one action', async () => {
  const ui = app();
  let submissions = 0;
  ui.submitForm = async () => { submissions++; return { signature: `tx${submissions}` }; };
  ui.showFlash = () => {};
  await ui.runOptimization();
  assert.equal(submissions, 2);
  assert.equal(ui.optimizePlan, null);
});

test('uncertain submission stops and requires a fresh preview', async () => {
  const ui = app();
  let submissions = 0;
  ui.submitForm = async () => { submissions++; throw new Error('timeout'); };
  await ui.runOptimization();
  await ui.runOptimization();
  assert.equal(submissions, 1);
  assert.equal(ui.optimizePlan, null);
  assert.match(ui.optimizeError, /Check wallet activity/);
});

test('wallet switch and wallet lock stop further submissions', async () => {
  for (const status of [{ wallet_address: 'other' }, { wallet_address: 'wallet', wallet_locked: true }]) {
    const ui = app();
    ui.status = status;
    ui.submitForm = async () => assert.fail('must not submit');
    await ui.runOptimization();
    assert.match(ui.optimizeError, /Wallet locked or changed/);
  }
});

test('broadcast failure stops without silently retrying', async () => {
  const ui = app();
  ui.submitForm = async () => ({ signature: 'accepted', broadcast_error: 'offline' });
  await ui.runOptimization();
  assert.match(ui.optimizeError, /queued locally/);
  assert.equal(ui.optimizePlan, null);
});

test('hides the optimization suggestion once submitted inputs are pending', () => {
  const ui = app();
  ui.walletUtxoPage.total = 500;
  ui.status.funded_wallet_addresses = [
    { address: 'wallet', utxos: 500, spendable_utxos: 1 },
  ];

  assert.equal(ui.walletSpendableUtxoCount(), 1);
  assert.equal(ui.showOptimizeSuggestion(), false);
});

test('keeps the optimization suggestion when enough spendable inputs remain', () => {
  const ui = app();
  ui.walletUtxoPage.total = 900;
  ui.status.funded_wallet_addresses = [
    { address: 'wallet', utxos: 600, spendable_utxos: 450 },
    { address: 'wallet-2', utxos: 300, spendable_utxos: 100 },
  ];

  assert.equal(ui.walletSpendableUtxoCount(), 550);
  assert.equal(ui.showOptimizeSuggestion(), true);
});

test('falls back to the paged UTXO total for older status payloads', () => {
  const ui = app();
  ui.walletUtxoPage.total = 500;

  assert.equal(ui.walletSpendableUtxoCount(), 500);
  assert.equal(ui.showOptimizeSuggestion(), true);
});
