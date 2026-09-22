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
  const ui = context.window.iunaApp();
  ui.status = {
    wallet_locked: false,
    quantum_migration: { active: true, legacy_balance: 1_000_000, hybrid_balance: 0 },
  };
  ui.refresh = async () => {};
  ui.showFlash = () => {};
  return ui;
}

test('migration preview does not submit funds', async () => {
  const ui = app();
  const paths = [];
  ui.submitForm = async (path, fields) => {
    paths.push(path);
    assert.equal(fields.fee_per_byte, 1);
    return {
      preview: {
        transaction_id: 'ab'.repeat(32),
        input_count: 2,
        remaining_legacy_utxos: 3,
        bytes: 500,
        fee: 500,
        amount: 999_500,
      },
    };
  };

  await ui.previewQuantumMigration();

  assert.deepEqual(paths, ['/api/wallet/quantum-migration/preview']);
  assert.equal(ui.quantumMigrationPreview.remaining_legacy_utxos, 3);
});

test('confirmed migration submits the exact preview once', async () => {
  const ui = app();
  ui.quantumMigrationPreview = {
    transaction_id: 'cd'.repeat(32),
    rate: 2,
    fee: 1_000,
    remaining_legacy_utxos: 4,
  };
  let submissions = 0;
  ui.submitForm = async (path, fields) => {
    submissions += 1;
    assert.equal(path, '/api/wallet/quantum-migration/submit');
    assert.equal(fields.transaction_id, 'cd'.repeat(32));
    assert.equal(fields.max_fee, 1_000);
    return { transaction_id: fields.transaction_id, remaining_legacy_utxos: 4 };
  };

  await ui.submitQuantumMigration();
  await ui.submitQuantumMigration();

  assert.equal(submissions, 1);
  assert.equal(ui.quantumMigrationPreview, null);
});

test('uncertain migration submission requires a fresh preview', async () => {
  const ui = app();
  ui.quantumMigrationPreview = {
    transaction_id: 'ef'.repeat(32),
    rate: 1,
    fee: 500,
  };
  ui.submitForm = async () => { throw new Error('timeout'); };

  await ui.submitQuantumMigration();

  assert.equal(ui.quantumMigrationPreview, null);
  assert.match(ui.quantumMigrationError, /Check wallet activity/);
});

test('hybrid recipients never reuse selected legacy UTXOs', () => {
  const ui = app();
  ui.transferTo = `iuna1p${'q'.repeat(58)}`;
  ui.selectedTransferUtxos = ['legacy:0'];
  ui.selectedTransferUtxoAmounts = { 'legacy:0': 10 };
  ui.showSendAdvanced = true;

  ui.transferRecipientChanged();

  assert.equal(ui.hybridTransferRecipient(), true);
  assert.equal(ui.selectedTransferUtxos.length, 0);
  assert.equal(ui.showSendAdvanced, false);
});

test('migration status can identify the exact pending v2 transaction', () => {
  const ui = app();
  ui.status.quantum_migration.migration_pending = true;
  ui.status.quantum_migration.pending_transaction_id = '12'.repeat(32);

  assert.equal(ui.status.quantum_migration.pending_transaction_id, '12'.repeat(32));
});
