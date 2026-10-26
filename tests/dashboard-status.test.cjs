const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

function app(storage = new Map()) {
  const context = {
    window: {},
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
    },
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

test('navigation shortcuts stay contiguous with optional developer tabs', () => {
  const ui = app();

  ui.keepTrackOfMetrics = false;
  assert.deepEqual(
    Array.from(ui.allowedTabs()),
    ['dashboard', 'wallet', 'mining', 'p2p', 'chain', 'settings']
  );
  assert.equal(ui.navigationShortcutNumber('dashboard'), '1');
  assert.equal(ui.navigationShortcutNumber('wallet'), '2');
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

test('tour covers standard tabs, mining workers, burning, and Discord', () => {
  const ui = app();
  const steps = Array.from(ui.tourSteps());

  assert.deepEqual(steps.map((step) => step.tab ?? null), [
    null,
    'dashboard', 'dashboard',
    'wallet', 'wallet',
    'mining', 'mining', 'mining', 'mining',
    'p2p', 'p2p',
    'chain', 'chain',
    'settings', 'settings'
  ]);
  assert.match(steps[6].text, /More workers/);
  assert.match(steps[7].text, /Burning more/);
  assert.match(steps.at(-1).text, /Discord/);
  assert.equal(steps.at(-1).clickTarget, undefined);
});

test('each screen is shown without a blackout before its navigation prompt', () => {
  const steps = Array.from(app().tourSteps());
  for (const [tab, nextTarget] of [
    ['dashboard', 'wallet'],
    ['wallet', 'mining'],
    ['mining', 'p2p'],
    ['p2p', 'chain'],
    ['chain', 'settings'],
  ]) {
    const explanationIndex = steps.findIndex((step) => step.tab === tab && !step.clickTarget);
    const transitionIndex = steps.findIndex((step) => step.tab === tab && step.clickTarget === nextTarget);
    assert.ok(explanationIndex >= 0, `${tab} has an explanation step`);
    assert.ok(transitionIndex > explanationIndex, `${tab} explains the screen before navigation`);
  }
});

test('tour advances between screens only after the highlighted tab is clicked', () => {
  const ui = app();
  ui.tourActive = true;
  ui.showTourStep = () => {};

  ui.tourTargetClicked('wallet');
  assert.equal(ui.tourStepIndex, 0);

  ui.tourTargetClicked('dashboard');
  assert.equal(ui.tourStepIndex, 1);

  ui.tourTargetClicked('wallet');
  assert.equal(ui.tourStepIndex, 1);

  ui.nextTourStep();
  assert.equal(ui.tourStepIndex, 2);

  ui.tourTargetClicked('wallet');
  assert.equal(ui.tourStepIndex, 3);
});

test('finishing the tour closes it and resets its visual state', () => {
  const storage = new Map();
  const ui = app(storage);
  ui.tourActive = true;
  ui.tourSpotlightStyle = 'positioned';
  ui.tourCardStyle = 'positioned';

  ui.finishTour();

  assert.equal(ui.tourActive, false);
  assert.match(ui.tourSpotlightStyle, /-9999px/);
  assert.match(ui.tourCardStyle, /-9999px/);
  assert.equal(storage.get('iunaUiTourCompletedV1'), 'true');
});

test('closing the tour prevents it from being offered automatically again', () => {
  const storage = new Map();
  const ui = app(storage);
  ui.tourActive = true;

  ui.closeTour();

  assert.equal(ui.tourActive, false);
  assert.equal(storage.get('iunaUiTourCompletedV1'), 'true');

  const nextUi = app(storage);
  nextUi.authLoaded = true;
  nextUi.auth = { configured: true, authenticated: true };
  nextUi.config = { setup_complete: true };
  nextUi.networkHealthLoaded = true;
  nextUi.networkHealth = { state: 'healthy' };
  let starts = 0;
  nextUi.startTour = () => { starts += 1; };

  nextUi.maybeStartTour();

  assert.equal(starts, 0);
  assert.equal(nextUi.tourOfferChecked, true);
});

test('a previously closed tour can still be started manually', () => {
  const storage = new Map([['iunaUiTourCompletedV1', 'true']]);
  const ui = app(storage);
  ui.closeModals = () => {};
  ui.showTourStep = () => {};

  ui.startTour();

  assert.equal(ui.tourActive, true);
  assert.equal(ui.tourStepIndex, 0);
});

test('automatic tour is offered once only when setup is ready', () => {
  const ui = app();
  ui.authLoaded = true;
  ui.auth = { configured: true, authenticated: true };
  ui.config = { setup_complete: true };
  ui.networkHealthLoaded = true;
  ui.networkHealth = { state: 'healthy' };
  let starts = 0;
  ui.startTour = () => { starts += 1; };

  ui.maybeStartTour();
  ui.maybeStartTour();

  assert.equal(starts, 1);
  assert.equal(ui.tourOfferChecked, true);
});

test('first-run tour starts on the first ready health update after sync', () => {
  const ui = app();
  ui.authLoaded = true;
  ui.auth = { configured: true, authenticated: true };
  ui.config = { setup_complete: true };
  let starts = 0;
  ui.startTour = () => { starts += 1; };

  ui.networkHealthLoaded = false;
  ui.maybeStartTour();
  assert.equal(starts, 0);
  assert.equal(ui.tourOfferChecked, false);

  ui.networkHealthLoaded = true;
  ui.networkHealth = { state: 'syncing' };
  ui.maybeStartTour();
  assert.equal(starts, 0);
  assert.equal(ui.tourOfferChecked, false);

  ui.networkHealth = { state: 'healthy' };
  ui.maybeStartTour();
  ui.maybeStartTour();
  assert.equal(starts, 1);
  assert.equal(ui.tourOfferChecked, true);
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
