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

test('modern plugged-in computer reports the hash-miner preview boundary', () => {
  const result = recommendationFor({ cpu: 'strong', power: 'plugged', network: 'public' });

  assert.equal(result.mining.title, 'Hash miner not packaged yet');
  assert.equal(result.ticketMining.title, 'Quote a mining ticket');
  assert.equal(result.network.title, 'Choose Listening node');
  assert.match(result.network.copy, /9444/);
});

test('lightweight plugged-in computer validates and can use ticket mining', () => {
  const result = recommendationFor({ cpu: 'light', power: 'plugged', network: 'private' });

  assert.equal(result.mining.title, 'Run a validating node');
  assert.equal(result.ticketMining.title, 'Ticket mining works');
  assert.equal(result.network.title, 'Choose Non-listening node');
});

test('battery power overrides CPU mining recommendation', () => {
  for (const cpu of ['strong', 'light']) {
    const result = recommendationFor({ cpu, power: 'battery', network: 'private' });
    assert.equal(result.mining.title, 'Hash mining off');
    assert.equal(result.ticketMining.title, 'Ticket mining optional');
  }
});
