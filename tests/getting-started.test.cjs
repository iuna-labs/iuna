const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

function recommendationFor(input) {
  const context = { window: {} };
  vm.runInNewContext(
    readFileSync(require.resolve('../www/assets/iuna-getting-started.js'), 'utf8'),
    context
  );
  return context.window.iunaGettingStartedRecommendation(input);
}

test('modern plugged-in computer mines and follows max-EV', () => {
  const result = recommendationFor({ cpu: 'strong', power: 'plugged', network: 'public' });

  assert.equal(result.mining.title, 'Mining on');
  assert.equal(result.burning.title, 'Use the max-EV burn');
  assert.equal(result.network.title, 'Choose Listening node');
  assert.match(result.network.copy, /9444/);
});

test('lightweight plugged-in computer keeps burn low', () => {
  const result = recommendationFor({ cpu: 'light', power: 'plugged', network: 'private' });

  assert.equal(result.mining.title, 'Mining optional');
  assert.equal(result.burning.title, 'Keep the burn low');
  assert.equal(result.network.title, 'Choose Non-listening node');
});

test('battery power overrides CPU mining recommendation', () => {
  for (const cpu of ['strong', 'light']) {
    const result = recommendationFor({ cpu, power: 'battery', network: 'private' });
    assert.equal(result.mining.title, 'Mining off');
    assert.equal(result.burning.title, 'Burn only, start low');
  }
});
