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
