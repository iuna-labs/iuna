const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');

const root = join(__dirname, '..');
const source = (path) => readFileSync(join(root, path), 'utf8');
const dataModule = (text) => `data:text/javascript;base64,${Buffer.from(text).toString('base64')}`;

let core;

test.before(async () => {
  core = await import(dataModule(source('stats/stats-core.js')));
});

const at = (href) => new URL(href);

test('stats page reads its own origin when hosted and Compose otherwise', () => {
  assert.equal(core.resolveNode(at('https://stats.getiuna.org/')), 'https://stats.getiuna.org');
  assert.equal(core.resolveNode(at('http://localhost:8080/stats/')), 'http://127.0.0.1:18662');
  assert.equal(
    core.resolveNode(at('https://stats.getiuna.org/?node=node.example.org')),
    'https://node.example.org',
  );
  assert.equal(
    core.resolveNode(at('https://stats.getiuna.org/?node=http://127.0.0.1:18662/')),
    'http://127.0.0.1:18662',
  );
});

test('only bare http(s) origins are accepted as nodes', () => {
  for (const rejected of ['', 'javascript:alert(1)', 'ftp://node.example', 'https://node.example/api', 'https://user@node.example', 'https://node.example/?x=1']) {
    assert.equal(core.normalizeNode(rejected), null, rejected);
  }
  assert.equal(core.resolveNode(at('https://stats.getiuna.org/?node=javascript:alert(1)')), 'https://stats.getiuna.org');
});

test('the mining window falls back to a day', () => {
  assert.equal(core.resolveWindow(at('https://stats.getiuna.org/?window=week')), 'week');
  assert.equal(core.resolveWindow(at('https://stats.getiuna.org/?window=year')), 'day');
  assert.equal(core.statsUrl('https://stats.getiuna.org', 'all'), 'https://stats.getiuna.org/api/public/stats?window=all');
});

test('amounts, shares and intervals are formatted for the leaderboard', () => {
  assert.equal(core.formatIuna(4_207_680_100_000_000), '42,076,801');
  assert.equal(core.formatIuna(150_000_001), '1.50000001');
  assert.equal(core.formatIunaShort(123_456_789_000), '1,234.57');
  assert.equal(core.formatIunaShort(1_000_000), '0.01');
  assert.equal(core.formatIunaShort(1), '0.00000001');
  assert.equal(core.formatShare(core.share(1, 3)), '33.3%');
  assert.equal(core.formatShare(core.share(1, 10_000)), '<0.1%');
  assert.equal(core.share(5, 0), 0);
  assert.equal(core.shortAddress('ab'.repeat(32)), 'abababab…ababab');
  assert.equal(core.averageInterval({ heartbeats: 3, from_timestamp: 100, to_timestamp: 220 }), 60);
  assert.equal(core.averageInterval({ heartbeats: 1, from_timestamp: 100, to_timestamp: 100 }), null);
  assert.equal(core.formatDuration(61), '61s');
  assert.equal(core.formatDuration(300), '5m');
});
