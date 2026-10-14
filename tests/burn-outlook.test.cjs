const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

const indexHtml = readFileSync(require.resolve('../src/adapters/http/index_html.rs'), 'utf8');

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

test('burn outlook is hidden while automatic burn is off', () => {
  assert.match(indexHtml, /class="burn-outlook" x-show="miningEnabled"/);
});

test('burn save button is primary only while settings are dirty', () => {
  assert.match(indexHtml, /:class="burnAmountDirty \? 'primary' : 'subtle'">Save<\/button>/);
});

test('burn outlook shows the proposed share and single-draw return', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 1_000_000, wallet_eligible_burn: 200_000 } };
  ui.burnAmountDraft = '0.2';
  ui.feeEstimates.burn = { fee: 0 };
  ui.blocks = [{
    height: 1,
    reward: 2_000_000,
    finalizer_mode: 'ticket',
    finalizer_rank: 0,
    burn_bundles: [{ slot: 1 }],
  }];

  const outlook = ui.burnOutlook();
  assert.equal(outlook.otherBurn, 800_000);
  assert.equal(outlook.scenarioTotal, 1_000_000);
  assert.equal(outlook.chancePercent, 20);
  assert.equal(outlook.reward, 1_000_000);
  assert.equal(outlook.expectedPayout, 200_000);
  assert.equal(outlook.net, 0);
  assert.equal(outlook.level, 'positive');
  assert.equal(outlook.maxExpectedValueBurn, 94_427);
  assert.equal(Math.round(outlook.maxExpectedNet), 11_146);
  assert.equal(Math.round(outlook.excessBurn), 105_573);
  assert.match(ui.burnOutlookSizingLabel(), /max-EV point: IUNA 0\.094427/);
  assert.match(ui.burnOutlookSizingLabel(), /IUNA 0\.105573 of the draft is beyond/);
  assert.match(ui.burnOutlookSizingLabel(), /Break-even ceiling.*IUNA 0\.2/);
});

test('burn outlook distinguishes below, at, and above the max-EV point', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 800_000, wallet_eligible_burn: 0 } };
  ui.blocks = [{ height: 1, reward: 1_000_000, finalizer_mode: 'recovery', burn_bundles: [] }];

  ui.burnAmountDraft = '0.05';
  assert.equal(ui.burnOutlook().verdict, 'Below max-EV: more burn raises expected net');

  ui.burnAmountDraft = '0.094427';
  assert.equal(ui.burnOutlook().verdict, 'At max-EV');

  ui.burnAmountDraft = '0.2';
  assert.equal(ui.burnOutlook().verdict, 'Overburn: extra burn lowers expected profit');
});

test('max-EV action fills the burn draft without saving it', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 800_000, wallet_eligible_burn: 0 } };
  ui.blocks = [{ height: 1, reward: 1_000_000, finalizer_mode: 'recovery', burn_bundles: [] }];
  ui.burnAmountDraft = '0.05';
  let scheduled = 0;
  ui.scheduleFeeEstimates = () => { scheduled += 1; };

  assert.equal(ui.canUseMaxEvBurn(), true);
  ui.useMaxEvBurn();

  assert.equal(ui.burnAmountDraft, '0.094427');
  assert.equal(ui.burnAmountDirty, true);
  assert.equal(scheduled, 1);
  assert.equal(ui.canUseMaxEvBurn(), false);
});

test('burn outlook warns when the scenario pool exceeds the payout', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 2_000_000, wallet_eligible_burn: 0 } };
  ui.burnAmountDraft = '1';
  ui.blocks = [{ height: 1, reward: 1_000_000, finalizer_mode: 'recovery', burn_bundles: [] }];

  const outlook = ui.burnOutlook();
  assert.ok(Math.abs(outlook.chancePercent - 100 / 3) < 1e-9);
  assert.ok(outlook.net < 0);
  assert.equal(outlook.verdict, 'Do not burn: competition exceeds payout');
  assert.match(ui.burnOutlookSizingLabel(), /Sizing signal: 0 IUNA/);
});

test('burn outlook says the minimum dominates when there is no competing burn', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 500_000, wallet_eligible_burn: 500_000 } };
  ui.blocks = [{ height: 1, reward: 2_000_000, finalizer_mode: 'recovery', burn_bundles: [] }];

  ui.burnAmountDraft = '0.1';
  const small = ui.burnOutlook();
  ui.burnAmountDraft = '1';
  const large = ui.burnOutlook();

  assert.equal(small.chancePercent, 100);
  assert.equal(large.chancePercent, 100);
  assert.ok(small.net > large.net);
  assert.equal(large.maxExpectedValueBurn, 1);
  assert.equal(large.maxExpectedNet, 1_999_999);
  assert.equal(large.verdict, 'No competition: minimum burn wins');
  assert.match(ui.burnOutlookSizingLabel(), /use the minimum practical amount/);
});

test('near-certain chances retain enough precision to compare tiny competition', () => {
  const ui = app();
  ui.status = { mining: { eligible_burn_total: 1, wallet_eligible_burn: 0 } };
  ui.blocks = [{ height: 1, reward: 1_000_000, finalizer_mode: 'recovery', burn_bundles: [] }];

  ui.burnAmountDraft = '0.1';
  assert.equal(ui.burnOutlookPercentLabel(), '99.999%');
  assert.equal(ui.burnOutlook().verdict, 'Overburn: extra burn lowers expected profit');

  ui.burnAmountDraft = '1';
  assert.equal(ui.burnOutlookPercentLabel(), '99.9999%');
  assert.ok(ui.burnOutlook().net < 0);
});
