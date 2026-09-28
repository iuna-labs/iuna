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

test('marks every node-owned address as mine', () => {
  const ui = app();
  ui.status = {
    wallet_address: 'legacy',
    wallet_receive_address: 'current',
    wallet_owned_addresses: ['legacy', 'rotated', 'current'],
    funded_wallet_addresses: [],
  };

  assert.equal(ui.addressLabel('rotated'), 'rotated (me)');
  assert.equal(ui.shortAddressLabel('current'), 'current (me)');
  assert.equal(ui.addressLabel('someone-else'), 'someone-else');
});

test('keeps contact names while marking an owned address', () => {
  const ui = app();
  ui.status = { wallet_owned_addresses: ['owned'], funded_wallet_addresses: [] };
  ui.addressBook = { owned: 'Savings' };

  assert.equal(ui.addressLabel('owned'), 'Savings (me)');
});

test('recognizes the primary address when the owned-address list is absent or empty', () => {
  for (const wallet_owned_addresses of [undefined, []]) {
    const ui = app();
    ui.status = {
      wallet_address: 'legacy',
      wallet_owned_addresses,
      funded_wallet_addresses: [],
    };

    assert.equal(ui.shortAddressLabel('legacy'), 'legacy (me)');
  }
});
