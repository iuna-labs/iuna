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

test('colors the last-block card by age', () => {
  const ui = app();

  for (const [ageMs, expected] of [
    [20 * 60 * 1000, 'good'],
    [20 * 60 * 1000 + 1, 'warning'],
    [60 * 60 * 1000, 'warning'],
    [60 * 60 * 1000 + 1, 'bad'],
  ]) {
    ui.networkHealth = { last_block_age_ms: ageMs };
    assert.equal(ui.dashboardBlockState(), expected);
  }
});

test('counts only mining-eligible peers as healthy', () => {
  const ui = app();
  const now = Date.now();
  ui.networkHealth = { local_height: 42, local_tip_hash: 'local-tip' };
  const peer = (overrides = {}) => ({
    last_error: null,
    last_known_height: 42,
    last_known_tip_hash: 'local-tip',
    last_success_ms: now,
    banned_until_ms: null,
    ...overrides,
  });

  ui.peers = [
    peer(),
    peer({ last_success_ms: now - 20 * 60 * 1000 - 1 }),
    peer({ banned_until_ms: now + 60_000 }),
    peer({ last_error: 'offline' }),
    peer({ last_known_height: null }),
    peer({ last_success_ms: null }),
    peer({ last_known_tip_hash: 'fork-tip' }),
  ];

  assert.equal(ui.healthyPeers().length, 1);
  assert.equal(ui.peerStatus(ui.peers.at(-1)), 'forked');
});
