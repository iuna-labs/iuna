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

test('navigation shortcuts stay contiguous in every interface mode', () => {
  const ui = app();

  ui.uiMode = 'basic';
  ui.keepTrackOfMetrics = false;
  assert.deepEqual(
    Array.from(ui.allowedTabs()),
    ['dashboard', 'wallet', 'p2p', 'chain', 'settings']
  );
  assert.equal(ui.navigationShortcutNumber('dashboard'), '1');
  assert.equal(ui.navigationShortcutNumber('wallet'), '2');
  assert.equal(ui.navigationShortcutNumber('p2p'), '3');

  ui.keepTrackOfMetrics = true;
  assert.deepEqual(
    Array.from(ui.allowedTabs()),
    ['dashboard', 'wallet', 'p2p', 'chain', 'metrics', 'leaderboards', 'settings']
  );
  assert.equal(ui.navigationShortcutNumber('metrics'), '5');
  assert.equal(ui.navigationShortcutNumber('settings'), '7');

  ui.uiMode = 'advanced';
  ui.keepTrackOfMetrics = false;
  assert.deepEqual(
    Array.from(ui.allowedTabs()),
    ['dashboard', 'wallet', 'mining', 'p2p', 'chain', 'settings']
  );
  assert.equal(ui.navigationShortcutNumber('mining'), '3');
  assert.equal(ui.navigationShortcutNumber('settings'), '6');

  ui.keepTrackOfMetrics = true;
  assert.deepEqual(
    Array.from(ui.allowedTabs()),
    ['dashboard', 'wallet', 'mining', 'p2p', 'chain', 'metrics', 'leaderboards', 'settings']
  );
  assert.equal(ui.navigationShortcutNumber('metrics'), '6');
  assert.equal(ui.navigationShortcutNumber('settings'), '8');
});

test('command-number switches screens and command release hides hints', () => {
  const ui = app();
  ui.authLoaded = true;
  ui.auth = { configured: true, authenticated: true };
  ui.config = { setup_complete: true, keep_track_of_metrics: false };
  ui.networkHealthLoaded = false;
  const selected = [];
  ui.setTab = (tab) => selected.push(tab);
  ui.closeModals = () => {};
  let prevented = false;

  ui.handleNavigationKeydown({ key: 'Meta', code: 'MetaLeft', metaKey: true });
  assert.equal(ui.commandKeyHeld, true);
  ui.handleNavigationKeydown({
    key: '2',
    code: 'Digit2',
    metaKey: true,
    preventDefault: () => { prevented = true; },
  });
  assert.deepEqual(selected, ['wallet']);
  assert.equal(prevented, true);

  ui.handleNavigationKeyup({ key: 'Meta', metaKey: false });
  assert.equal(ui.commandKeyHeld, false);
});
