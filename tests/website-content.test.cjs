const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');

const publicFiles = [
  'README.md',
  'www/index.html',
  'www/downloads.html',
];

function content(path) {
  return readFileSync(join(__dirname, '..', path), 'utf8');
}

test('public protocol copy uses Heartbeat, Ledger, and expenditure vocabulary', () => {
  const combined = publicFiles.map(content).join('\n');

  for (const expected of [
    'Heartbeat Chain',
    'Ledger Chain',
    'Proof of Expenditure',
    'candidate mainnet',
    '9444',
  ]) {
    assert.match(combined, new RegExp(expected));
  }

  for (const obsolete of [
    /experimental L1/i,
    /proof[- ]of[- ]burn/i,
    /verifiable delay/i,
    /devnet-v2/i,
    /protocol-v2/i,
  ]) {
    assert.doesNotMatch(combined, obsolete);
  }
});

test('canonical documentation has no parallel v2 identity', () => {
  const combined = [content('docs/operator.md'), content('docs/protocol.md')].join('\n');

  assert.doesNotMatch(combined, /devnet-v2/i);
  assert.doesNotMatch(combined, /protocol-v2/i);
  assert.doesNotMatch(combined, /operator-v2/i);
});

test('protocol visualization matches the canonical timing and vocabulary', () => {
  const visualization = content('visualizations/iuna-protocol.html');

  assert.match(visualization, /60 s Heartbeat/);
  assert.match(visualization, /D = 10 heartbeats/);
  assert.match(visualization, /last payout H\+1,448/);
  assert.match(visualization, /probability = pct \/ 100 \/ 10/);
  assert.match(visualization, /expectedMinutes = expectedHeartbeats/);
  assert.doesNotMatch(visualization, /\bL1\b|\bL2\b|8\.64[089]|10 s heartbeat|5 min L2|PLAN\.md/i);
});

test('container and deployment defaults match the canonical toolchain and ports', () => {
  const deployment = [
    content('Dockerfile.local-testnet'),
    content('Dockerfile.node'),
    content('KNOWN_NODES.txt'),
    content('deployment.sh'),
    content('config/deployment.yml'),
    content('src-tauri/tauri.conf.json'),
  ].join('\n');

  assert.match(deployment, /rust:1\.99\.1-bookworm/);
  assert.match(content('Dockerfile.node'), /EXPOSE 8080 18662 9444/);
  const compose = content('docker-compose.yml');
  assert.match(compose, /127\.0\.0\.1:8081:8080/);
  assert.match(compose, /127\.0\.0\.1:18662:18662/);
  assert.match(compose, /--wallet-endpoint/);
  assert.match(compose, /127\.0\.0\.1:8082:8080/);
  assert.match(compose, /IUNA_INSECURE_OPERATOR_API: "1"/);
  assert.doesNotMatch(deployment, /rust:1\.88(?:\.0)?-bookworm|18661/i);
});

test('downloads page lists the release artifacts and the source build', () => {
  const downloads = content('www/downloads.html');

  assert.match(downloads, /cargo run --release/);
  assert.doesNotMatch(downloads, /iuna-mainnet-candidate/);
  assert.match(downloads, /src="\/assets\/iuna-downloads\.js"/);
  assert.match(downloads, /data-downloads-version="\$\{IUNA_VERSION\}"/);
  assert.match(downloads, /Candidate mainnet/);
  assert.doesNotMatch(downloads, /<script>/);
  for (const file of ['linux-x86_64.tar.gz', 'linux-aarch64.tar.gz', 'macos-aarch64-desktop.app.zip', 'windows-x86_64-desktop-setup.exe']) {
    assert.match(content('www/assets/iuna-downloads.js'), new RegExp(file.replace(/\./g, '\\.')));
  }
});

test('homepage keeps its original calls to action and links to the live wallet host', () => {
  const html = content('www/index.html');
  const heroStart = html.indexOf('<header class="hero">');
  const calloutStart = html.indexOf('<aside class="candidate-callout"');
  const hero = html.slice(heroStart, calloutStart);

  assert.match(hero, />Download iuna<\/a>/);
  assert.doesNotMatch(html, /getting-started/i);
  assert.match(hero, />Why iuna<\/a>/);
  assert.match(hero, />How it works<\/a>/);
  assert.match(hero, />Roadmap<\/a>/);
  assert.doesNotMatch(hero, />Open wallet<\/a>/);
  assert.doesNotMatch(html, /wallet\.getiuna\.com/);
  assert.match(html, /https:\/\/wallet\.getiuna\.org\//);
  assert.doesNotMatch(html, /Temporary cost/);
});

test('desktop credentials cross into the node UI without entering request URLs', () => {
  const desktop = content('desktop/index.html');
  const nodeUi = content('www/assets/iuna-ui.js');

  assert.match(desktop, /desktop_operator_token/);
  assert.match(desktop, /#operator-token=/);
  assert.match(nodeUi, /location\.hash/);
  assert.match(nodeUi, /sessionStorage\.setItem\(["']iunaOperatorToken["']/);
  assert.match(nodeUi, /history\.replaceState/);
});

test('only the bundled launcher can read the desktop operator token', () => {
  const launcher = JSON.parse(content('src-tauri/capabilities/desktop-launcher.json'));
  const shared = JSON.parse(content('src-tauri/capabilities/default.json'));

  assert.match(content('src-tauri/build.rs'), /"desktop_operator_token"/);
  assert.deepEqual(launcher.permissions, ['allow-desktop-operator-token']);
  assert.equal(launcher.local, true);
  assert.equal(launcher.remote, undefined);
  assert.ok(!shared.permissions.includes('allow-desktop-operator-token'));
  assert.ok(!JSON.stringify(shared.permissions).includes('shell:allow-execute'));
});

test('the desktop launcher trusts its own node instead of any server on the port', () => {
  const desktop = content('desktop/index.html');
  const shell = content('src-tauri/src/main.rs');
  const node = content('src/main.rs');

  assert.doesNotMatch(desktop, /fetch\(/);
  assert.match(shell, /const NODE_READY_LINE: &str = "operator API: http:\/\/127\.0\.0\.1:8080\/api\/status";/);
  assert.match(node, /http_addr: "127\.0\.0\.1:8080"/);
  assert.match(node, /println!\("operator API: http:\/\/\{http_addr\}\/api\/status"\);/);
  assert.ok(node.indexOf('println!("operator API:') > node.indexOf('TcpListener::bind(options.http_addr)'));
});

test('node UI restores the signed desktop update flow', async () => {
  const calls = [];
  const window = {
    location: { hash: '', pathname: '/', search: '' },
    __TAURI__: {
      core: {
        invoke: async (command) => {
          calls.push(command);
          return command === 'check_desktop_update' ? '0.5.0' : null;
        },
      },
    },
  };
  vm.runInNewContext(content('www/assets/iuna-ui.js'), {
    window,
    fetch: async () => ({
      ok: true,
      json: async () => ({ version: '0.5.0', url: 'https://getiuna.org/downloads/' }),
    }),
    clearTimeout: () => {},
    setTimeout: () => 1,
  });
  const app = window.iunaApp();
  app.status = { app_version: '0.4.70' };
  app.notify = () => {};

  await app.checkLatestRelease();
  assert.equal(app.appVersionLabel(), 'v0.4.70');
  assert.equal(app.updateAvailable(), true);
  assert.equal(app.latestRelease.desktopReady, true);
  await app.openLatestRelease();
  assert.equal(app.desktopUpdateModalOpen, true);
  await app.installDesktopUpdate();
  assert.deepEqual(calls, ['check_desktop_update', 'install_desktop_update']);

  const html = content('www/node.html');
  assert.match(html, /class="version-panel"/);
  assert.match(html, /Install and restart/);
});

test('node UI preserves the product screens while using the canonical protocol', () => {
  const html = content('www/node.html');
  const controller = content('www/assets/iuna-ui.js');
  const combined = `${html}\n${controller}`;

  for (const expected of [
    'Initial Setup',
    'Dashboard',
    'Wallet',
    'Heartbeat mining',
    'P2P',
    'Heartbeat chain',
    'Ledger chain',
    'Metrics',
    'Settings',
    'Proof of Work',
    'Worker threads',
    'Proof of Expenditure',
    'Create your wallet',
    'Connected nodes',
    'Block details',
    'Heartbeat ticket purchases',
    'Ledger transactions',
    'No activity to chart yet',
  ]) {
    assert.match(combined, new RegExp(expected));
  }

  assert.match(html, /target every 60 seconds/);
  assert.match(html, /target every 10 Heartbeats/);
  assert.match(html, /step="0\.00000001"/);
  assert.match(controller, /filter\(\(block\) => block\.has_ledger_block\)/);
  assert.match(controller, /\/api\/blocks\/\$\{block\.block_hash\}/);
  assert.match(controller, /workers: this\.normalizedMiningWorkers\(\)/);
  assert.match(controller, /\/api\/ledger-mining/);
  assert.match(controller, /\/api\/mempool/);
  assert.match(controller, /observeActivitySentinel/);
  assert.match(html, /Automatic budget/);
  assert.match(html, /Next ticket/);
  assert.match(html, /x-show="!status\.ledger_mining_enabled"/);
  assert.match(html, /x-show="status\.ledger_mining_enabled"/);
  assert.match(controller, /100_000_000/);
  assert.match(controller, /nextSetupStep/);
  assert.match(html, /toggle-switch/);
  assert.match(html, /PoW difficulty/);
  assert.doesNotMatch(html, />Quote</);
  assert.doesNotMatch(html, /PoW \/ SPoW|Heartbeat \/ Transaction/);
  assert.doesNotMatch(html, /Bootstrap peer|Non-listening node|Listening node/);
  assert.doesNotMatch(combined, /VDF|quantum|proof[- ]of[- ]burn|PoB|devnet-v2|protocol-v2/i);
});

test('node UI converts IUNA amounts at consensus precision', () => {
  const window = {};
  vm.runInNewContext(content('www/assets/iuna-ui.js'), { window });
  const app = window.iunaApp();

  assert.equal(app.parseAmount('1.00000001'), 100_000_001);
  assert.equal(app.amountLabel(100_000_001), '1.00000001');
  assert.throws(() => app.parseAmount('0.000000001'));
});

test('initial setup can be completed without trapping a read-only wallet', () => {
  const values = new Map();
  const window = { location: { hash: '', pathname: '/', search: '' } };
  const localStorage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  vm.runInNewContext(content('www/assets/iuna-ui.js'), {
    window,
    localStorage,
    clearTimeout: () => {},
    setTimeout: () => 1,
  });
  const app = window.iunaApp();
  app.status = { wallet_unlocked: false, node_instance_id: '00112233445566778899aabbccddeeff' };
  app.setTab = (tab) => { app.tab = tab; };

  app.nextSetupStep();
  app.nextSetupStep();
  app.nextSetupStep();
  assert.equal(app.setupStep, 3);
  app.completeSetup();

  assert.equal(values.get('iunaSetupComplete:00112233445566778899aabbccddeeff'), 'true');
  assert.equal(app.showSetup, false);
  assert.equal(app.tab, 'dashboard');
});

test('initial setup completion is remembered per node data directory', () => {
  const { app } = nodeApp();
  app.status = { node_instance_id: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' };
  app.completeSetup();
  assert.equal(app.setupCompleted(), true);

  app.status.node_instance_id = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
  assert.equal(app.setupCompleted(), false);
});

test('unlocking closes the login even when the following refresh fails', async () => {
  const { app } = nodeApp();
  app.setupReady = true;
  app.showSetup = false;
  app.status = { wallet_public_key: '11'.repeat(32), wallet_unlocked: false };
  app.syncLoginVisibility();
  assert.equal(app.showLogin, true);
  app.postJson = async () => ({ created: false, public_key: '11'.repeat(32) });
  app.refresh = async () => {};
  app.notify = () => {};
  app.walletAccess.password = 'correct horse battery';

  await app.accessWallet();

  assert.equal(app.showLogin, false);
  assert.equal(app.status.wallet_unlocked, true);
});

test('an existing locked wallet opens the login modal after restart', () => {
  const { app } = nodeApp();
  app.setupReady = true;
  app.showSetup = false;
  app.status = {
    wallet_public_key: '11'.repeat(32),
    wallet_unlocked: false,
  };
  app.syncLoginVisibility();
  assert.equal(app.showLogin, true);
  assert.equal(app.shortcutsAvailable(), false);

  app.status.wallet_unlocked = true;
  app.syncLoginVisibility();
  assert.equal(app.showLogin, false);

  const html = content('www/node.html');
  assert.match(html, /id="login-title">Unlock iuna/);
  assert.match(html, /class="setup-modal login-modal"/);
  assert.match(html, /class="setup-overlay login-overlay"/);
  assert.match(html, /\.login-overlay \{ padding: 40px 22px; overflow-y: auto; \}/);
  assert.match(html, /\.login-modal \{ width: min\(340px, 100%\); max-height: calc\(100vh - 80px\); \}/);
  assert.match(html, /\.login-modal h1 \{ margin-bottom: 22px; \}/);
  assert.match(html, /\.login-form label \{ gap: 8px; \}/);
  assert.match(html, /class="login-form"/);
  assert.match(html, /x-ref="loginPassword"/);
  assert.match(html, /autocomplete="current-password"/);
});

test('wallet setup requires recovery phrase verification', () => {
  const window = { location: { hash: '', pathname: '/', search: '' } };
  vm.runInNewContext(content('www/assets/iuna-ui.js'), {
    window,
    clearTimeout: () => {},
    setTimeout: () => 1,
    crypto: globalThis.crypto,
  });
  const app = window.iunaApp();
  app.setupStep = 2;
  app.status = { wallet_unlocked: true };
  app.walletAccess.recoveryPhrase = Array.from({ length: 24 }, (_, index) => `word${index + 1}`).join(' ');
  app.notify = () => {};

  assert.equal(app.canContinueSetup(), false);
  app.startRecoveryCheck();
  const indices = [...app.walletAccess.confirmationIndices];
  assert.equal(new Set(indices).size, 3);
  assert.ok(indices.every((index) => index >= 1 && index <= 24));
  const answers = indices.map((index) => `word${index}`);
  app.walletAccess.confirmationWords = [answers[0], answers[1], 'wrong'];
  app.verifyRecoveryPhrase();
  assert.equal(app.canContinueSetup(), false);

  app.walletAccess.confirmationWords = answers;
  app.verifyRecoveryPhrase();
  assert.equal(app.canContinueSetup(), true);
  assert.equal(app.walletAccess.recoveryPhrase, '');
});

// Builds the node UI component outside the browser with only the globals each test needs.
function nodeApp(globals = {}) {
  const values = new Map();
  const window = { location: { hash: '', pathname: '/', search: '' } };
  vm.runInNewContext(content('www/assets/iuna-ui.js'), {
    window,
    localStorage: { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) },
    navigator: { platform: 'MacIntel' },
    crypto: globalThis.crypto,
    BigInt,
    clearTimeout: () => {},
    setTimeout: () => 1,
    ...globals,
  });
  const app = window.iunaApp();
  const messages = [];
  app.notify = (message, kind = 'success') => messages.push({ message, kind });
  app.$nextTick = () => {};
  app.$refs = {};
  app.setTab = (tab) => { app.tab = tab; };
  return { app, messages, values };
}

test('transfers are only signed after review, within the spendable balance', () => {
  const { app, messages } = nodeApp();
  app.status = { management_enabled: true, wallet_unlocked: true, wallet_spendable_balance: 100_000_000 };
  app.transfer.recipient = 'not-a-key';
  app.transfer.amount = '0.5';
  app.reviewTransaction();
  assert.equal(app.pendingTransfer, null);
  assert.match(messages.at(-1).message, /64-character/);

  app.transfer.recipient = 'AB'.repeat(32);
  app.transfer.amount = '1';
  app.reviewTransaction();
  assert.equal(app.pendingTransfer, null, 'amount plus fee may not exceed the spendable balance');

  app.useMaximumAmount();
  assert.equal(app.transfer.amount, '0.99999');
  app.reviewTransaction();
  assert.deepEqual({ ...app.pendingTransfer }, { recipient: 'ab'.repeat(32), amount: 99_999_000, fee: 1_000, remaining: 0 });

  app.handleKeydown({ key: 'Escape' });
  assert.equal(app.pendingTransfer, null);
});

test('command-number shortcuts open sidebar screens only when no dialog is open', () => {
  const { app } = nodeApp();
  let prevented = 0;
  const press = (extra) => app.handleKeydown({ key: '3', code: 'Digit3', metaKey: true, preventDefault: () => { prevented += 1; }, ...extra });
  press();
  assert.equal(app.tab, 'mining');
  press({ shiftKey: true, code: 'Digit2', key: '2' });
  assert.equal(app.tab, 'mining', 'modified shortcuts are ignored');
  app.pendingTransfer = { recipient: '', amount: 1, fee: 1 };
  press({ code: 'Digit1', key: '1' });
  assert.equal(app.tab, 'mining', 'shortcuts are disabled behind the confirm dialog');
  assert.equal(prevented, 1);
});

test('mining controls separate manual tickets from continuous read-only state', async () => {
  const { app } = nodeApp();
  app.ticket.duration = 10;
  app.getJson = async (path) => ({
    duration: 10,
    recommended_budget: 250_000_000,
    current_minimum_budget: 200_000_000,
    path,
  });
  await app.quoteTicket();
  assert.equal(app.ticket.continuousQuote, app.ticket.quote);
  const cachedQuote = app.ticket.continuousQuote;
  await app.quoteContinuousTicket(true, false);
  assert.notEqual(app.ticket.continuousQuote, cachedQuote, 'continuous mode can force a fresh quote');

  app.status = { ledger_mining_enabled: true, heartbeat_height: 24, ledger_last_ticket_height: 20 };
  assert.match(app.ledgerMiningMessage(), /submits a recommended Ledger ticket every 10 Heartbeats/);
  assert.equal(app.ledgerNextTicketLabel(), 'Heartbeat #30');
  app.status.heartbeat_height = 30;
  assert.equal(app.ledgerNextTicketLabel(), 'Due now');
  app.status.ledger_last_ticket_height = null;
  assert.equal(app.ledgerNextTicketLabel(), 'As soon as possible');

  app.status.ledger_mining_enabled = false;
  assert.match(app.ledgerMiningMessage(), /Submit a ticket manually below/);
});

test('metrics use the median interval and per-block difficulty', () => {
  const { app } = nodeApp();
  const block = (height, timestamp, target) => ({ height, timestamp, target });
  const limit = '0000003f' + 'f'.repeat(56);
  const harder = '0000001f' + 'f'.repeat(56);
  app.status = { pow_limit: limit };
  // Genesis is excluded and one long outage does not move the median.
  app.heartbeats = [block(0, 0, limit), block(1, 1000, limit), block(2, 1060, limit), block(3, 1120, harder), block(4, 30000, harder), block(5, 30060, harder)];
  assert.equal(app.averageHeartbeatSeconds(), 60);
  const difficulty = app.chartSeries('difficulty').map((point) => point.y);
  assert.equal(difficulty[0], 1);
  assert.ok(Math.abs(difficulty.at(-1) - 2) < 0.01);
  assert.equal(app.chartBounds('interval').high, 600, 'outages are drawn capped at ten minutes');
});

test('metrics ranges and protocol charts use per-Heartbeat projection rows', () => {
  const { app, values } = nodeApp();
  const limit = '0000003f' + 'f'.repeat(56);
  app.status = { pow_limit: limit };
  app.metricsRows = [
    { height: 2, timestamp: 1060, interval_seconds: 60, target: limit, ledger_transaction_count: 3, chain_bytes: 900, heartbeat_bytes: 450, active_tickets: 2, ticket_weight: 20, minted: 200, utxos: 7, tracked_tickets: 4, ledger_blocks: 1 },
    { height: 1, timestamp: 1000, interval_seconds: 60, target: limit, ledger_transaction_count: 1, chain_bytes: 450, heartbeat_bytes: 450, active_tickets: 1, ticket_weight: 10, minted: 100, utxos: 5, tracked_tickets: 2, ledger_blocks: 0 },
  ];
  assert.equal(app.metricsPath(), '/api/metrics?limit=100');
  app.refreshMetrics = () => {};
  app.setMetricsRange(1000);
  assert.equal(app.metricsPath(), '/api/metrics?limit=1000');
  assert.equal(values.get('iunaMetricsRange'), '1000');
  app.setMetricsRange('all');
  assert.equal(app.metricsPath(), '/api/metrics');
  assert.equal(app.chartSeries('transactions').map((point) => point.y).join(','), '1,3');
  assert.equal(app.chartSeries('ticketBudget').map((point) => point.y).join(','), '10,20');
  assert.equal(app.chartSeries('chainBytes').at(-1).y, 900);
  assert.equal(app.chartSeries('supply').at(-1).y, 200);
  assert.equal(app.chartSeries('utxos').at(-1).y, 7);
  assert.equal(app.metricsCharts().length, 10);

  const html = content('www/node.html');
  for (const copy of ['Last 100', 'Last 1000', 'Total IUNA supply']) {
    assert.match(html, new RegExp(copy));
  }
  const controller = content('www/assets/iuna-ui.js');
  for (const copy of ['Ledger transactions', 'Compact chain size', 'Mining tickets', 'Ticket budget', 'UTXO set size']) {
    assert.match(controller, new RegExp(copy));
  }
  assert.doesNotMatch(controller, /per Heartbeat/);
});

test('large metrics ranges cache calculations and only draw useful SVG detail', () => {
  const { app } = nodeApp();
  const limit = '0000003f' + 'f'.repeat(56);
  app.status = { pow_limit: limit };
  app.metricsRows = Array.from({ length: 5_000 }, (_, index) => ({
    height: 5_000 - index,
    timestamp: 1_000 + (5_000 - index) * 60,
    interval_seconds: index === 2_500 ? 600 : 60,
    target: limit,
    chain_bytes: index * 100,
  }));

  const series = app.chartSeries('interval');
  assert.equal(app.chartSeries('interval'), series, 'the projected series is reused');
  const displayed = app.chartDisplaySeries('interval');
  assert.ok(displayed.length <= 300, `expected at most 300 SVG points, got ${displayed.length}`);
  assert.ok(displayed.some((point) => point.y === 600), 'bucket extrema preserve short spikes');
  assert.equal(app.chartPolyline('interval'), app.chartPolyline('interval'), 'the SVG projection is reused');
  assert.equal(app.averageHeartbeatSeconds(), 60, 'statistics still use all selected Heartbeats');
});

test('metric axes use round ticks and compact labels', () => {
  const { app } = nodeApp();
  app.metricsRows = [
    { height: 2, chain_bytes: 987, minted: 12_345_678_900 },
    { height: 1, chain_bytes: 123, minted: 10_000_000_000 },
  ];

  assert.equal(app.chartTicks('chainBytes').join(','), '1000,500,0');
  assert.equal(app.chartAxisValueLabel('chainBytes', 1_536), '1.5KiB');
  assert.equal(app.chartAxisValueLabel('chainBytes', 500_000_000), '477MiB');
  assert.equal(app.chartAxisValueLabel('supply', 12_345_678_900), '123.46');
  assert.equal(app.chartAxisValueLabel('transactions', 1_500_000), '1.5M');
  assert.equal(app.chartAxisValueLabel('difficulty', 1.2345), '1.23×');
  assert.match(app.chartGridPath('chainBytes'), /^M6 /);

  const html = content('www/node.html');
  assert.match(html, /chartAxisValueLabel\(chart\.kind, tick\)/);
  assert.match(html, /chartGridPath\(chart\.kind\)/);
});

test('dashboard cards report stale chains and isolated nodes', () => {
  const { app } = nodeApp();
  app.status = { network: 'iuna-mainnet-candidate', wallet_unlocked: true, heartbeat_mining_enabled: false, ledger_mining_enabled: true };
  app.peers = [];
  const now = Math.floor(Date.now() / 1000);
  app.heartbeats = [{ height: 9, timestamp: now - 30 }];
  assert.equal(app.dashboardState('heartbeat'), 'good');
  app.heartbeats = [{ height: 9, timestamp: now - 3600 }];
  assert.equal(app.dashboardState('heartbeat'), 'bad');
  assert.equal(app.dashboardState('p2p'), 'warn');
  assert.equal(app.dashboardState('heartbeatMining'), 'idle');
  assert.equal(app.dashboardState('ledgerMining'), 'good');
  const html = content('www/node.html');
  const dashboard = html.slice(html.indexOf('<section class="dashboard"'), html.indexOf(`<section x-show="tab === 'wallet'">`));
  assert.equal((dashboard.match(/<article class="dashboard-card/g) || []).length, 6);
  for (const label of ['Current balance', 'Heartbeat Mining', 'Ledger Mining', 'Heartbeat chain', 'Ledger chain', 'P2P']) {
    assert.match(dashboard, new RegExp(`>${label}<`, 'i'));
  }
  app.status.heartbeat_height = 9;
  app.peers = [{ heartbeat_height: 10 }];
  assert.equal(app.syncTarget(), null, 'one block behind is normal propagation');
  app.peers = [{ heartbeat_height: 50 }, { heartbeat_height: null }];
  assert.equal(app.syncTarget(), 50);
  assert.equal(app.syncPercent(), 18);
  assert.equal(app.networkLabel(), 'Syncing 18%');
  assert.equal(app.syncLabel(), 'Syncing 18%');
  assert.equal(app.networkHeight(), 50);
  assert.equal(app.dashboardState('heartbeat'), 'warn', 'an old tip while syncing is not a stall');
  app.status.heartbeat_height = 50;
  assert.equal(app.syncLabel(), 'Up to date');
  assert.equal(app.networkLabel(), 'Connected');
  app.peers = [];
  assert.equal(app.syncLabel(), 'No peers');
  assert.equal(app.networkHeight(), null);
});

test('dashboard uses single-line ages and wallet activity grows in pages', () => {
  const { app } = nodeApp();
  const now = Math.floor(Date.now() / 1000);
  app.heartbeats = [{ height: 20, timestamp: now - 12 }];
  app.ledgerBlocks = [{ height: 19, timestamp: now - 75 }];
  assert.ok([12, 13].includes(Number(app.heartbeatAgeLabel().split(' ')[0])));
  assert.equal(app.ledgerAgeLabel(), '1 min ago');
  app.ledgerBlocks = [{ height: 19, timestamp: now - (3 * 3600 + 120) }];
  assert.equal(app.ledgerAgeLabel(), '3 h ago');

  app.activity = {
    pending: [],
    confirmed: Array.from({ length: 60 }, (_, index) => ({ id: String(index) })),
  };
  assert.equal(app.visibleActivityItems().length, 25);
  assert.equal(app.activityHasMore(), true);
  app.loadMoreActivity();
  assert.equal(app.visibleActivityItems().length, 50);
});

test('dashboard keeps long balances and the Heartbeat mining label inside their cards', () => {
  const { app } = nodeApp();
  app.status = { wallet_balance: 9_007_199_254_740_991 };
  assert.equal(app.balanceValueStyle(), 'font-size:7.5px');

  const html = content('www/node.html');
  assert.match(html, /:style="balanceValueStyle\(\)"/);
  assert.match(html, /\.dashboard-card\.heartbeat-mining \.dashboard-label \{[^}]*white-space: nowrap/);
});

test('wallet activity includes mining rewards and UTXOs open separately', async () => {
  const { app } = nodeApp();
  assert.equal(app.activityLabel({ kind: 'heartbeat_reward' }), 'Heartbeat reward');
  assert.equal(app.activityLabel({ kind: 'ledger_reward' }), 'Ledger reward');
  assert.equal(app.activitySign({ kind: 'heartbeat_reward' }), '+');
  assert.equal(app.activitySign({ kind: 'ledger_reward' }), '+');
  app.getJson = async () => ({ utxos: [{ transaction_id: 'aa', output_index: 0 }] });
  await app.openUtxos();
  assert.equal(app.showUtxos, true);
  assert.equal(app.activity.utxos.length, 1);

  const html = content('www/node.html');
  assert.match(html, /class="utxo-details-button" @click="openUtxos\(\)">View details/);
  assert.match(html, /id="utxo-title">Unspent outputs/);
  assert.match(html, /status\.wallet_utxos/);
  assert.doesNotMatch(html, /activity\?\.utxos\.length \?\? status\.utxos/);
  assert.doesNotMatch(html, /<details class="tx-card" style="margin-top:12px" x-show="activity\?\.utxos/);
});

test('wallet activity filters each protocol action and keeps filtered infinite scroll', () => {
  const { app } = nodeApp();
  const make = (kind, count) => Array.from({ length: count }, (_, index) => ({ kind, id: `${kind}-${index}` }));
  app.activity = {
    pending: [],
    confirmed: [
      ...make('sent', 30),
      ...make('ticket', 30),
      ...make('heartbeat_reward', 30),
      ...make('ledger_reward', 30),
    ],
  };
  app.activityFilters = { transfers: false, tickets: true, heartbeatRewards: false, ledgerRewards: false };
  assert.equal(app.activityCode({ kind: 'sent' }), 'TX');
  assert.equal(app.activityCode({ kind: 'ticket' }), 'TI');
  assert.equal(app.activityCode({ kind: 'heartbeat_reward' }), 'HR');
  assert.equal(app.activityCode({ kind: 'ledger_reward' }), 'LR');
  assert.equal(app.activityLabel({ kind: 'sent' }), 'Sent');
  assert.equal(app.filteredActivityItems().length, 30);
  assert.equal(app.visibleActivityItems().length, 25);
  assert.equal(app.activityHasMore(), true);
  app.loadMoreActivity();
  assert.equal(app.visibleActivityItems().length, 30);
  assert.equal(app.activityHasMore(), false);

  const html = content('www/node.html');
  for (const label of ['Transfers', 'Tickets', 'Heartbeat rewards', 'Ledger rewards']) {
    assert.match(html, new RegExp(`>${label}<`));
  }
  for (const type of ['transfers', 'tickets', 'heartbeatRewards', 'ledgerRewards']) {
    assert.match(html, new RegExp(`activity-filter ${type}`));
  }
  assert.match(html, /:class="\[activityType\(item\), \{pending: item\.pending\}\]"/);
  assert.match(html, /class="activity-title" x-text="activityLabel\(item\)"/);
  assert.match(html, /class="utxo-details-button"/);
  assert.match(html, /x-ref="activitySentinel"/);
});

test('chain overview labels miners and identifies the local wallet', () => {
  const { app } = nodeApp();
  const mine = '00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff';
  app.status = { wallet_public_key: mine };
  assert.equal(app.minerLabel(mine), 'ccddeeff (me)');
  assert.equal(app.minerLabel('ff'.repeat(32)), 'ffffffff');
  assert.equal(app.minerLabel('invalid'), '-');

  const html = content('www/node.html');
  assert.match(html, /minerLabel\(block\.miner_public_key\)/);
  assert.match(html, /minerLabel\(block\.ledger_miner_public_key \|\| block\.miner_public_key\)/);
  assert.equal(app.chainView, 'ledger');
  app.setChainView('heartbeat');
  assert.equal(app.chainView, 'heartbeat');
  app.setChainView('ledger');
  assert.equal(app.chainView, 'ledger');
  app.setChainView('invalid');
  assert.equal(app.chainView, 'ledger');
  assert.match(html, /class="panel chain-panel"/);
  assert.match(html, /class="chain-tabs" role="tablist"/);
  assert.match(html, />Heartbeat<\/button>/);
  assert.match(html, />Ledger<\/button>/);
  assert.doesNotMatch(html, /<h2>Heartbeat chain<\/h2>/);
  assert.doesNotMatch(html, /<h2>Ledger chain<\/h2>/);
  assert.match(html, /@scroll\.passive="handleChainScroll\('heartbeat', \$event\)"/);
  assert.match(html, /@scroll\.passive="handleChainScroll\('ledger', \$event\)"/);
  assert.match(html, /class="chain-block chain-block-building"/);
  assert.match(html, /class="chain-block-main"/);
  assert.match(html, /class="chain-block-meta"/);
  assert.match(html, /block\.ticket_count/);
  assert.match(html, /block\.ledger_transaction_count/);
  assert.match(html, /button\.chain-block:hover, button\.chain-block:focus-visible/);
  assert.match(html, /\.chain-block\.selected \{ border-color: var\(--green\); background: #202616;/);
  assert.match(html, /\.chain-block\.new-block \{ animation: chain-block-arrive/);
  assert.doesNotMatch(html, /class="chain-height">#/);
  const heartbeatBuilding = html.slice(
    html.indexOf('aria-label="Next Heartbeat being searched"'),
    html.indexOf('<template x-for="block in heartbeats"'),
  );
  assert.match(heartbeatBuilding, /mempool\.mining_tickets/);
  assert.doesNotMatch(heartbeatBuilding, /worker|chain-miner/i);
  const ledgerBuilding = html.slice(
    html.indexOf('aria-label="Next Ledger block being searched"'),
    html.indexOf('<template x-for="block in ledgerBlocks"'),
  );
  assert.match(ledgerBuilding, /mempool\.ledger_transactions/);
  assert.doesNotMatch(ledgerBuilding, /chain-miner/i);
});

test('chain rails append older heartbeat pages and derive older Ledger blocks', async () => {
  const { app } = nodeApp();
  app.status = { ledger_blocks: 2 };
  app.heartbeats = [{ block_hash: 'h3', height: 3, has_ledger_block: true }];
  app.syncLedgerBlocks();
  app.heartbeatPage = { limit: 2, busy: false, hasMore: true };
  app.getJson = async (path) => {
    assert.equal(path, '/api/heartbeats?limit=2&before_height=3');
    return [
      { block_hash: 'h2', height: 2, has_ledger_block: false },
      { block_hash: 'h1', height: 1, has_ledger_block: true },
    ];
  };

  await app.loadMoreHeartbeats('ledger');
  assert.equal(app.heartbeats.map((block) => block.height).join(','), '3,2,1');
  assert.equal(app.ledgerBlocks.map((block) => block.height).join(','), '3,1');
  assert.equal(app.ledgerBlocks.map((block) => block.ledger_height).join(','), '2,1');
  assert.equal(app.heartbeatPage.hasMore, true);
});

test('a short Ledger rail auto-loads until horizontal scrolling is possible', async () => {
  const { app } = nodeApp();
  app.tab = 'chain';
  app.chainView = 'ledger';
  app.heartbeats = [{ block_hash: 'h2', height: 2 }];
  app.heartbeatPage = { limit: 100, busy: false, hasMore: true };
  app.$refs = { ledgerRail: { clientWidth: 1000, scrollWidth: 950 } };
  let loads = 0;
  app.loadMoreHeartbeats = async (chain) => {
    assert.equal(chain, 'ledger');
    loads += 1;
    app.heartbeats.push({ block_hash: 'h1', height: 1 });
    app.$refs.ledgerRail.scrollWidth = 1100;
  };

  await app.ensureChainRailScrollable('ledger');
  assert.equal(loads, 1);
  assert.equal(app.heartbeats.length, 2);
});

test('newest chain selections follow their chain while older selections stay fixed', async () => {
  const { app } = nodeApp();
  const heartbeat1 = { block_hash: 'h1', height: 1 };
  const heartbeat2 = { block_hash: 'h2', height: 2, has_ledger_block: true };
  app.heartbeats = [heartbeat2, heartbeat1];
  app.ledgerBlocks = [heartbeat2];
  app.getJson = async (path) => ({ path });

  await app.selectBlock(heartbeat2, 'heartbeat');
  assert.equal(app.selectedBlockFollowsLatest, true);
  const heartbeat3 = { block_hash: 'h3', height: 3 };
  app.heartbeats = [heartbeat3, heartbeat2, heartbeat1];
  assert.equal(app.reconcileSelectedBlock(), true);
  assert.equal(app.selectedBlock.block_hash, 'h3');

  await app.selectBlock(heartbeat1, 'heartbeat');
  assert.equal(app.selectedBlockFollowsLatest, false);
  app.heartbeats = [{ block_hash: 'h4', height: 4 }, heartbeat3, heartbeat2];
  assert.equal(app.reconcileSelectedBlock(), false);
  assert.equal(app.selectedBlock.block_hash, 'h1');

  await app.selectBlock(heartbeat2, 'ledger');
  const heartbeat5 = { block_hash: 'h5', height: 5 };
  app.heartbeats = [heartbeat5, ...app.heartbeats];
  assert.equal(app.reconcileSelectedBlock(), false);
  assert.equal(app.selectedBlock.block_hash, 'h2');
  const ledger2 = { block_hash: 'h6', height: 6, has_ledger_block: true };
  app.ledgerBlocks = [ledger2, heartbeat2];
  assert.equal(app.reconcileSelectedBlock(), true);
  assert.equal(app.selectedBlock.block_hash, 'h6');
});

test('block details show Heartbeat and Ledger reward breakdowns', () => {
  const html = content('www/node.html');
  assert.match(html, /Heartbeat reward/);
  assert.match(html, /Ledger reward/);
  assert.match(html, /rewards\.heartbeat\.total/);
  assert.match(html, /rewards\.ledger\?\.subsidy/);
  assert.match(html, /rewards\.ledger\?\.ledger_transaction_fees/);
});

test('new chain heads slide both rails back to the newest block when already at the start', () => {
  const { app } = nodeApp();
  const calls = [];
  const rail = {
    scrollWidth: 640,
    scrollLeft: 0,
    scrollTo: (options) => calls.push(options),
  };
  app.$refs = { heartbeatRail: rail };
  app.$nextTick = (callback) => callback();
  app.scheduleChainHeadSlide(
    'heartbeatRail',
    { width: 500, atStart: true },
    'old-head',
    'new-head',
  );
  assert.equal(rail.scrollLeft, 140);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].left, 0);
  assert.equal(calls[0].behavior, 'smooth');

  calls.length = 0;
  app.scheduleChainHeadSlide(
    'heartbeatRail',
    { width: 500, atStart: false },
    'new-head',
    'newer-head',
  );
  assert.equal(calls.length, 0);

  const html = content('www/node.html');
  assert.match(html, /x-ref="heartbeatRail"/);
  assert.match(html, /x-ref="ledgerRail"/);
  assert.match(html, /\.chain-detail \.detail-kv \{ display: grid; grid-template-columns: 90px/);
});

test('chain mempool opens full Ledger transaction and Heartbeat ticket details', async () => {
  const { app } = nodeApp();
  app.mempool = {
    ledger_transactions: [{ id: 'tx', version: 1, total_output: 12, fee: 3, fee_split: { heartbeat_miner: { value: 1, owner: null }, ledger_winner: { value: 2, owner: null } }, inputs: [{ transaction_id: 'in', output_index: 0 }], outputs: [{ owner: 'alice', value: 12 }] }],
    mining_tickets: [{ id: 'ticket', version: 1, budget: 38, fee: 2, duration: 10, lifecycle_heartbeats: 19, mining_public_key: 'miner', inputs: [], change: { owner: 'change', value: 5 } }],
  };
  const items = app.mempoolItems();
  assert.equal(
    items.map((item) => `${item.kind}:${item.amount}`).join(','),
    'Ledger transaction:12,Mining ticket:38',
  );
  app.openTransactionModal(items[0]);
  assert.equal(app.selectedTransaction.item.id, 'tx');
  assert.equal(app.selectedTransaction.source, 'Mempool');
  assert.equal(app.transactionModalInputs().length, 1);
  assert.equal(
    app.transactionModalOutputs().map((output) => `${output.label}:${output.value}`).join(','),
    'Output 1:12,Heartbeat miner fee:1,Ledger winner fee:2',
  );
  assert.equal(app.transactionModalOutputs()[1].owner, null);
  assert.match(app.transactionModalOutputs()[1].note, /known once mined/);
  app.closeModals();
  assert.equal(app.selectedTransaction, null);
  app.openTransactionModal(items[1]);
  assert.equal(app.transactionModalOutputs().map((output) => output.label).join(','), 'Ticket budget,Change,Inclusion fee');
  assert.equal(app.transactionModalOutputs().reduce((total, output) => total + output.value, 0), 45);
  assert.match(app.transactionModalOutputs()[0].note, /in 19 slices/);
  assert.equal(app.transactionModalOutputs()[0].owner, undefined);
  assert.equal(app.transactionModalOutputs()[0].status, 'locked');
  assert.equal(app.transactionModalOutputs()[1].owner, 'change');

  await app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.source, 'Mempool');
  // Missing from the capped preview does not mean gone: only the node can tell.
  app.mempool = { ledger_transactions: [], mining_tickets: [] };
  app.getJson = async () => ({ id: 'ticket' });
  await app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.source, 'Mempool');
  app.getJson = async () => { throw Object.assign(new Error('failed'), { status: 500 }); };
  await app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.source, 'Mempool');
  app.getJson = async () => { throw Object.assign(new Error('gone'), { status: 404 }); };
  await app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.source, 'No longer in mempool');
  assert.equal(app.selectedTransaction.item.id, 'ticket');

  const html = content('www/node.html');
  const chainStart = html.indexOf(`<section x-show="tab === 'chain'">`);
  const metricsStart = html.indexOf(`<section x-show="tab === 'metrics'">`);
  const chainHtml = html.slice(chainStart, metricsStart);
  assert.match(chainHtml, /class="panel mempool-panel"/);
  assert.match(html, /class="mempool-strip"/);
  assert.match(chainHtml, /x-show="mempoolItems\(\)\.length > 0"/);
  assert.match(chainHtml, /@click="openMempoolModal\(item\)"/);
  assert.match(html, /aria-labelledby="transaction-modal-title"/);
  assert.match(html, /transactionModalInputs\(\)/);
  assert.match(html, /transactionModalOutputs\(\)/);
  assert.match(html, /class="utxo-flow"/);
});

test('Heartbeat ticket purchases reuse the transaction modal with their fee', () => {
  const { app } = nodeApp();
  app.selectedBlock = { height: 42 };
  const ticket = {
    id: 'ticket',
    version: 1,
    budget: 100,
    duration: 10,
    lifecycle_heartbeats: 19,
    fee: 3,
    mining_public_key: 'miner',
    lottery_seed: 'seed',
    inputs: [],
    change: null,
  };
  app.openHeartbeatTicketModal(ticket);
  assert.equal(app.selectedTransaction.source, 'Heartbeat 42');
  assert.equal(app.selectedTransaction.item.kind, 'Mining ticket');
  assert.equal(app.selectedTransaction.item.fee, 3);
  assert.equal(app.transactionModalOutputs()[0].value, 100);

  const html = content('www/node.html');
  assert.match(html, /@click="openHeartbeatTicketModal\(ticket\)"/);
  assert.doesNotMatch(html, /<details class="tx-card">\s*<summary><span class="pill">Ticket/);
});

test('mempool modals load full detail and report items that left', async () => {
  const { app } = nodeApp();
  const item = { id: 'tx', kind: 'Ledger transaction', amount: 12, input_count: 1, output_count: 1 };
  app.getJson = async (path) => {
    assert.equal(path, '/api/mempool/tx');
    assert.equal(app.selectedTransaction.loading, true);
    return { inputs: [{ transaction_id: 'in', output_index: 0 }], outputs: [{ owner: 'alice', value: 12 }] };
  };
  await app.openMempoolModal(item);
  assert.equal(app.selectedTransaction.loading, false);
  assert.equal(app.selectedTransaction.item.kind, 'Ledger transaction');
  assert.equal(app.transactionModalInputs().length, 1);
  assert.equal(app.transactionModalOutputs()[0].owner, 'alice');

  app.getJson = async () => { throw Object.assign(new Error('not in the mempool'), { status: 404 }); };
  await app.openMempoolModal(item);
  assert.equal(app.selectedTransaction.source, 'No longer in mempool');
  assert.equal(app.selectedTransaction.loading, false);

  // A failed lookup says nothing about the item, and a summary must not pass for a total.
  app.getJson = async () => { throw Object.assign(new Error('failed'), { status: 500 }); };
  await app.openMempoolModal({ ...item, fee: 3, fee_split: { heartbeat_miner: { value: 1 }, ledger_winner: { value: 2 } } });
  assert.equal(app.selectedTransaction.source, 'Mempool');
  assert.equal(app.selectedTransaction.detailMissing, true);
  assert.equal(app.transactionModalTotals().out, null);

  // The next poll recovers a live item, and a late answer cannot touch another modal.
  app.mempool = { ledger_transactions: [{ id: 'tx' }], mining_tickets: [] };
  app.getJson = async () => ({ inputs: [{ transaction_id: 'in', output_index: 0, value: 15 }], outputs: [{ owner: 'alice', value: 12 }] });
  await app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.detailMissing, false);
  assert.equal(app.transactionModalTotals().balanced, true);

  app.mempool = { ledger_transactions: [], mining_tickets: [] };
  let reject;
  app.getJson = () => new Promise((_, fail) => { reject = fail; });
  const late = app.reconcileSelectedTransaction();
  app.openBlockTransactionModal({ id: 'tx', inputs: [], outputs: [] });
  reject(Object.assign(new Error('gone'), { status: 404 }));
  await late;
  assert.match(app.selectedTransaction.source, /^Heartbeat/);

  let finish;
  app.getJson = () => new Promise((resolve) => { finish = resolve; });
  const pending = app.openMempoolModal(item);
  app.closeTransactionModal();
  finish({ inputs: [] });
  await pending;
  assert.equal(app.selectedTransaction, null);
});

test('every transaction modal balances and shows its fee as an output', async () => {
  const input = (value) => ({ transaction_id: 'in', output_index: 0, public_key: 'key', signature: 'sig', value });
  const split = (heartbeat, winner, owners = [null, null]) => ({
    heartbeat_miner: { value: heartbeat, owner: owners[0] },
    ledger_winner: { value: winner, owner: owners[1] },
  });
  const ledger = (fields) => ({
    id: 'tx',
    version: 1,
    total_output: 90,
    fee: 7,
    inputs: [input(60), input(37)],
    outputs: [{ owner: 'alice', value: 80 }, { owner: 'change', value: 10 }],
    ...fields,
  });
  const ticket = (fields) => ({
    id: 'ticket',
    version: 1,
    budget: 80,
    duration: 10,
    lifecycle_heartbeats: 19,
    fee: 7,
    mining_public_key: 'miner',
    lottery_seed: 'seed',
    inputs: [input(97)],
    change: { owner: 'change', value: 10 },
    ...fields,
  });
  const summary = ({ inputs, outputs, change, lottery_seed: _, ...rest }) => rest;
  const opened = {
    'mempool Ledger transaction': async (app) => {
      const full = ledger({ fee_split: split(3, 4) });
      app.getJson = async () => full;
      await app.openMempoolModal({ ...summary(full), kind: 'Ledger transaction', amount: 90 });
    },
    'block Ledger transaction': (app) => app.openBlockTransactionModal(ledger({ fee_split: split(3, 4, ['miner', 'winner']) })),
    'mempool Mining ticket': async (app) => {
      const full = ticket();
      app.getJson = async () => full;
      await app.openMempoolModal({ ...summary(full), kind: 'Mining ticket', amount: 80 });
    },
    'block Mining ticket': (app) => app.openHeartbeatTicketModal(ticket({ fee_recipient: 'miner' })),
  };
  for (const [name, open] of Object.entries(opened)) {
    const { app } = nodeApp();
    await open(app);
    const totals = app.transactionModalTotals();
    assert.equal(totals.in, 97, name);
    assert.equal(totals.out, 97, name);
    assert.equal(totals.balanced, true, name);
    const fees = app.transactionModalOutputs().filter((output) => output.kind === 'fee');
    assert.equal(fees.reduce((total, output) => total + output.value, 0), 7, name);
  }
});

test('mempool fees stay visible while the detail loads or fails', async () => {
  const { app } = nodeApp();
  const item = {
    id: 'tx',
    kind: 'Ledger transaction',
    fee: 3,
    fee_split: { heartbeat_miner: { value: 1, owner: null }, ledger_winner: { value: 2, owner: null } },
  };
  let seen;
  app.getJson = async () => {
    seen = app.transactionModalOutputs().map((output) => output.value).join(',');
    assert.equal(app.transactionModalTotals().in, null);
    throw new Error('not in the mempool');
  };
  await app.openMempoolModal(item);
  assert.equal(seen, '1,2');
  assert.equal(app.transactionModalOutputs().map((output) => output.value).join(','), '1,2');
  // Without inputs there is nothing to compare, which is not a zero.
  assert.equal(app.transactionModalTotals().in, null);
  assert.equal(app.transactionModalTotals().out, null);
  assert.equal(app.transactionModalTotals().balanced, false);
});

test('a locked wallet cannot submit the backup form as an unlock', async () => {
  const { app } = nodeApp();
  const posted = [];
  app.postJson = async (_, body) => { posted.push(body); return {}; };
  app.refresh = async () => {};
  app.status = { wallet_unlocked: true };
  app.selectWalletMode('backup');
  app.status = { wallet_unlocked: false };
  // A poll that has not run yet must not turn the unlock into a backup.
  app.walletNeedsCreation = () => false;
  app.walletAccess.password = 'a password';
  await app.accessWallet();
  assert.equal(posted[0].action, 'unlock');

  app.status = { wallet_unlocked: false };
  app.selectWalletMode('backup');
  app.walletAccess.recoveryPhrase = 'twenty four words';
  app.syncLoginVisibility();
  assert.equal(app.walletAccess.mode, '');
  assert.equal(app.walletAccess.recoveryPhrase, '');
  assert.match(content('www/node.html'), /:disabled="walletAccess\.busy \|\| !browserCanManage\(\)">Show recovery phrase/);
});

test('a fee share of zero is not listed as an output', () => {
  const { app } = nodeApp();
  app.openBlockTransactionModal({
    id: 'tx',
    total_output: 5,
    fee: 1,
    fee_split: { heartbeat_miner: { value: 0, owner: 'miner' }, ledger_winner: { value: 1, owner: 'winner' } },
    inputs: [{ transaction_id: 'in', output_index: 0, value: 6 }],
    outputs: [{ owner: 'alice', value: 5 }],
  });
  assert.equal(app.transactionModalOutputs().map((output) => output.label).join(','), 'Output 1,Ledger winner fee');
  assert.equal(app.transactionModalTotals().balanced, true);
});

test('release gates and wallet caching cover what the review found', () => {
  assert.doesNotMatch(content('e2e/iuna_e2e.py'), /TCP_TEST/);
  const nginx = content('nginx.conf');
  const wallet = nginx.slice(nginx.indexOf('server_name wallet.getiuna.com'));
  assert.match(wallet, /add_header Cache-Control "no-cache" always;/);
  const hosted = nginx.slice(nginx.indexOf('location /wallet/'), nginx.indexOf('location = /node.html'));
  assert.match(hosted, /add_header Cache-Control "no-cache" always;/);
});

test('sidebar buttons have room to center their longest label', () => {
  assert.match(content('www/node.html'), /\.nav-button \{ padding-inline: 0; \}/);
});

test('elements toggled by x-show keep their layout in a class', () => {
  // Alpine removes an inline `display` when it shows an element, which silently drops grid gaps.
  const tags = content('www/node.html').match(/<[a-z][a-z0-9]*\b(?:"[^"]*"|'[^']*'|[^>"'])*>/g);
  const fragile = tags.filter((tag) => /\sx-show=/.test(tag) && /style="[^"]*display\s*:/.test(tag));
  assert.deepEqual(fragile, []);
  assert.match(content('www/node.html'), /\.stacked-form \{ display: grid; gap: 10px; \}/);
});

test('block details leave Ledger presence to the Ledger section', () => {
  const html = content('www/node.html');
  assert.doesNotMatch(html, /'Included' : 'None'/);
  assert.match(html, /No Ledger block is attached to this Heartbeat\./);
});

test('block transaction modals name the loaded Heartbeat, not the followed head', () => {
  const { app } = nodeApp();
  app.selectedBlock = { height: 43 };
  app.selectedBlockDetail = { height: 42 };
  app.openHeartbeatTicketModal({ id: 'ticket', budget: 19, lifecycle_heartbeats: 19, inputs: [], change: null });
  assert.equal(app.selectedTransaction.source, 'Heartbeat 42');

  app.openBlockTransactionModal({
    id: 'tx',
    total_output: 12,
    fee: 3,
    fee_split: { heartbeat_miner: { value: 1, owner: 'miner' }, ledger_winner: { value: 2, owner: 'winner' } },
    inputs: [],
    outputs: [{ owner: 'alice', value: 12 }],
  });
  const feeOutputs = app.transactionModalOutputs().slice(1);
  assert.equal(feeOutputs.map((output) => `${output.owner}:${output.value}`).join(','), 'miner:1,winner:2');
  assert.ok(feeOutputs.every((output) => !output.note));
  assert.equal(app.selectedTransaction.source, 'Heartbeat 42');
  assert.equal(app.selectedTransaction.item.kind, 'Ledger transaction');
  assert.equal(app.selectedTransaction.item.amount, 12);
  app.reconcileSelectedTransaction();
  assert.equal(app.selectedTransaction.source, 'Heartbeat 42');

  const html = content('www/node.html');
  assert.match(html, /@click="openBlockTransactionModal\(transaction\)"/);
  assert.doesNotMatch(html, /<details class="tx-card">\s*<summary><span class="pill">Transaction/);
});

test('transaction modal keeps keyboard focus inside and returns it on close', () => {
  const trigger = { focused: 0, focus() { this.focused += 1; } };
  const first = { focus() { document.activeElement = first; } };
  const last = { focus() { document.activeElement = last; } };
  const document = { activeElement: trigger };
  const { app } = nodeApp({ document });
  app.$nextTick = (callback) => callback();
  app.$refs = {
    transactionModal: { focus() {}, querySelectorAll: () => [first, last] },
  };
  app.openTransactionModal({ id: 'tx', kind: 'Ledger transaction', inputs: [], outputs: [] });

  let prevented = 0;
  document.activeElement = last;
  app.trapTransactionModalFocus({ shiftKey: false, preventDefault() { prevented += 1; } });
  assert.equal(document.activeElement, first);
  app.trapTransactionModalFocus({ shiftKey: true, preventDefault() { prevented += 1; } });
  assert.equal(document.activeElement, last);
  assert.equal(prevented, 2);

  app.closeTransactionModal();
  assert.equal(trigger.focused, 1);
  assert.equal(app.selectedTransaction, null);

  const html = content('www/node.html');
  assert.match(html, /@keydown\.tab="trapTransactionModalFocus\(\$event\)"/);
  // Bindings read a cleared selection on close, so the overlay must not fade out.
  assert.match(html, /x-show="selectedTransaction" x-transition:enter\.opacity /);
});

test('contacts are validated, de-duplicated and kept in browser storage', () => {
  const { app, values } = nodeApp();
  app.addContact('Alice', 'CD'.repeat(32));
  app.addContact('Alice twice', 'cd'.repeat(32));
  app.addContact('Bob', 'short');
  assert.equal(app.contacts.map((contact) => contact.name).join(','), 'Alice');
  assert.equal(JSON.parse(values.get('iunaContacts'))[0].address, 'cd'.repeat(32));
  assert.equal(app.contactName('cd'.repeat(32)), 'Alice');
});

test('activity rows open the shared transaction modal with loaded details', async () => {
  const { app } = nodeApp();
  const requests = [];
  const ticket = { id: 'ti', version: 1, budget: 38, lifecycle_heartbeats: 19, duration: 10, inputs: [], change: null, fee: 1 };
  const transaction = { id: 'tx', version: 1, inputs: [{}], outputs: [{ owner: 'a', value: 12 }], total_output: 12, fee: 3, fee_split: null };
  app.getJson = async (url) => {
    requests.push(url);
    if (url.startsWith('/api/heartbeats')) return [{ height: 7, block_hash: 'hash' }];
    if (url === '/api/blocks/hash') return { tickets: [ticket], ledger: { transactions: [transaction] } };
    if (url === '/api/mempool/pending') return { ...transaction, id: 'pending' };
    throw new Error(`unexpected ${url}`);
  };

  await app.openActivityModal({ kind: 'sent', id: 'tx', heartbeat_height: 7, amount: 5 });
  assert.equal(requests[0], '/api/heartbeats?limit=1&before_height=8');
  assert.equal(app.selectedTransaction.source, 'Heartbeat 7');
  assert.equal(app.selectedTransaction.item.kind, 'Ledger transaction');
  assert.equal(app.selectedTransaction.item.amount, 12);
  assert.equal(app.transactionModalInputs().length, 1);

  await app.openActivityModal({ kind: 'ticket', id: 'ti', heartbeat_height: 7, amount: 38 });
  assert.equal(app.selectedTransaction.item.kind, 'Mining ticket');
  assert.equal(app.transactionModalOutputs()[0].label, 'Ticket budget');

  await app.openActivityModal({ kind: 'received', id: 'pending', pending: true, amount: 5 });
  assert.equal(app.selectedTransaction.source, 'Mempool');
  assert.equal(app.selectedTransaction.item.amount, 12);

  await app.openActivityModal({ kind: 'ticket', id: 'gone', heartbeat_height: 7, amount: 38 });
  assert.equal(app.selectedTransaction.detailMissing, true);
  assert.equal(app.transactionModalOutputs().length, 0);

  requests.length = 0;
  app.status = { wallet_public_key: 'me' };
  await app.openActivityModal({ kind: 'ledger_reward', id: 'hash', heartbeat_height: 7, amount: 9 });
  assert.equal(requests.length, 0);
  assert.equal(app.selectedTransaction.item.kind, 'Ledger reward');
  assert.equal(
    app.transactionModalOutputs().map((output) => `${output.label}:${output.owner}:${output.value}`).join(','),
    'Ledger reward:me:9',
  );

  const html = content('www/node.html');
  assert.match(html, /class="activity-card"[^>]*@click="openActivityModal\(item\)"/);
  assert.match(html, /\.activity-card \{[^}]*cursor: pointer/);
  assert.match(html, /@click\.stop="copyValue\(item\.id/);
});

test('clicking an activity public key opens the contact editor', () => {
  const { app, values } = nodeApp();
  const address = 'ab'.repeat(32);
  app.openContactEditor(address.toUpperCase());
  assert.equal(app.contactModalOpen, true);
  assert.equal(app.contactEditingAddress, null);
  assert.equal(app.contactForm.address, address);
  app.contactForm.name = 'Alice';
  app.saveContactEditor();
  assert.equal(app.contactModalOpen, false);
  assert.equal(JSON.parse(values.get('iunaContacts'))[0].name, 'Alice');

  app.openContactEditor(address);
  assert.equal(app.contactEditingAddress, address);
  assert.equal(app.contactForm.name, 'Alice');
  app.contactForm.name = 'Alice Updated';
  app.saveContactEditor();
  assert.equal(app.contactName(address), 'Alice Updated');

  const html = content('www/node.html');
  assert.match(html, /@click\.stop="openContactEditor\(item\.counterparty\)"/);
  assert.match(html, /id="contact-editor-title"/);
});

test('browser wallet talks to its own origin when hosted and to the local node otherwise', async () => {
  const core = await import(`data:text/javascript;base64,${Buffer.from(content('wallet/wallet-core.js')).toString('base64')}`);
  assert.equal(core.defaultApiEndpoint({ hostname: 'wallet.getiuna.com', origin: 'https://wallet.getiuna.com' }), 'https://wallet.getiuna.com');
  assert.equal(core.defaultApiEndpoint({ hostname: '127.0.0.1', origin: 'http://127.0.0.1:8080' }), 'http://127.0.0.1:18662');
  assert.equal(core.defaultApiEndpoint({ hostname: 'localhost', origin: 'http://localhost:18200' }), 'http://127.0.0.1:18662');
});
