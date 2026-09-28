const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { pathToFileURL } = require('node:url');

let core;

test.before(async () => {
  const source = readFileSync(require.resolve('../wallet/wallet-core.js'), 'utf8');
  core = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
});

test('migrates the single-wallet record into a named wallet collection', () => {
  const legacy = { address: 'ab'.repeat(32), ciphertext: 'encrypted' };
  const store = core.normalizeWalletStore(null, legacy);

  assert.equal(store.version, 2);
  assert.equal(store.wallets.length, 1);
  assert.equal(store.wallets[0].type, 'signing');
  assert.equal(store.wallets[0].record, legacy);
  assert.equal(store.activeId, store.wallets[0].id);
});

test('adds, selects, and removes multiple wallet types', () => {
  const signing = { id: 'signing', name: 'Daily', type: 'signing', publicKeyHex: '11'.repeat(32), record: { ciphertext: 'secret' } };
  const readonly = { id: 'readonly', name: 'Savings', type: 'readonly', publicKeyHex: '22'.repeat(32) };
  let store = core.upsertWallet(core.emptyWalletStore(), signing);
  store = core.upsertWallet(store, readonly);

  assert.deepEqual(store.wallets.map((wallet) => wallet.name), ['Daily', 'Savings']);
  assert.equal(store.activeId, 'readonly');
  assert.equal('record' in store.wallets[1], false);

  store = core.removeWallet(store, 'readonly');
  assert.equal(store.activeId, 'signing');
  assert.deepEqual(store.wallets.map((wallet) => wallet.id), ['signing']);
});

test('extends both hybrid branches until historical recovery addresses are found', () => {
  const externalAddresses = Array.from({ length: 21 }, (_, index) => ({ index, address: `external-${index}` }));
  const rewardAddresses = Array.from({ length: 20 }, (_, index) => ({ index, address: `reward-${index}` }));
  const expanded = core.nextRecoveryScanCounts({
    externalAddresses,
    rewardAddresses,
    recoveryAddresses: ['external-27'],
    gapLimit: 20,
  });

  assert.equal(expanded.externalCount, 42);
  assert.equal(expanded.rewardCount, 40);
  assert.deepEqual(expanded.unresolved, ['external-27']);

  const recovered = core.nextRecoveryScanCounts({
    externalAddresses: Array.from({ length: 41 }, (_, index) => ({ index, address: `external-${index}` })),
    rewardAddresses: Array.from({ length: 40 }, (_, index) => ({ index, address: `reward-${index}` })),
    recoveryAddresses: ['external-27'],
    gapLimit: 20,
  });
  assert.equal(recovered.externalCount, 41);
  assert.equal(recovered.rewardCount, 40);
  assert.deepEqual(recovered.unresolved, []);

  const rewardRecovered = core.nextRecoveryScanCounts({
    externalAddresses,
    rewardAddresses,
    recoveryAddresses: ['reward-27'],
    gapLimit: 20,
  });
  assert.equal(rewardRecovered.externalCount, 42);
  assert.equal(rewardRecovered.rewardCount, 40);
  assert.deepEqual(rewardRecovered.unresolved, ['reward-27']);
});

test('stops historical recovery cleanly at the derivation safety limit', () => {
  const addresses = Array.from({ length: 10 }, (_, index) => ({ index, address: `address-${index}` }));
  const result = core.nextRecoveryScanCounts({
    externalAddresses: addresses,
    rewardAddresses: addresses,
    recoveryAddresses: ['missing'],
    gapLimit: 20,
    maxAddresses: 10,
  });

  assert.equal(result.externalCount, 10);
  assert.equal(result.rewardCount, 10);
  assert.deepEqual(result.unresolved, ['missing']);
});

test('refuses to sign with a watch-only wallet', async () => {
  await assert.rejects(
    core.buildSignedTransfer({ wallet: { publicKeyHex: '22'.repeat(32) } }),
    /watch-only wallet cannot sign/,
  );
});

test('explains oversized transactions before submission', () => {
  const signature = 'aa'.repeat(64);
  const transaction = {
    kind: 'transfer',
    inputs: Array.from({ length: 205 }, (_, index) => ({
      outpoint: { txid: 'bb'.repeat(32), index },
      owner: 'cc'.repeat(32),
      signature,
    })),
    outputs: [
      { address: 'dd'.repeat(32), amount: 1 },
      { address: 'cc'.repeat(32), amount: 1 },
    ],
    fee: 1,
    signature,
  };

  assert.throws(
    () => core.ensureTransactionBodySize(transaction),
    /205 inputs, 65 KiB; maximum 64 KiB.*smaller amount.*consolidate/s,
  );
});

test('validates a configurable fee rate with a default minimum of one', () => {
  assert.equal(core.parseFeeRate('1'), 1n);
  assert.equal(core.parseFeeRate('25'), 25n);
  assert.throws(() => core.parseFeeRate('0'), /at least 1/);
  assert.throws(() => core.parseFeeRate('1.5'), /whole number/);
});

test('uses the selected fee rate when calculating a transfer', () => {
  const utxos = [{
    outpoint: { txid: 'aa'.repeat(32), index: 0 },
    output: { address: 'bb'.repeat(32), amount: 1_000_000 },
  }];
  const owner = 'bb'.repeat(32);
  const recipient = 'cc'.repeat(32);
  const standard = core.selectInputs(utxos, 100_000n, 1n, owner, recipient);
  const priority = core.selectInputs(utxos, 100_000n, 5n, owner, recipient);

  assert.equal(priority.fee, standard.fee * 5n);
});

test('recognizes hybrid Bech32m addresses without treating them as legacy keys', () => {
  const address = 'iuna1py9qlrnw6cm3mpz26hwwkww9spaa96zrw2g34gu0p4y3ea5cqhg0qa82x9s';
  const decoded = core.decodeVersionedAddress(address, 'iuna');
  assert.equal(decoded.version, 1);
  assert.equal(decoded.payloadHex.length, 64);
  assert.throws(() => core.decodeAddress(address, 'iuna'), /requires a legacy address/);
});

test('browser crypto derives the same rotating hybrid address vectors as the node', async () => {
  const modulePath = require.resolve('../wallet/crypto/iuna_wallet_crypto.js');
  const wasmPath = require.resolve('../wallet/crypto/iuna_wallet_crypto_bg.wasm');
  const quantum = await import(pathToFileURL(modulePath));
  await quantum.default({ module_or_path: readFileSync(wasmPath) });
  const addresses = JSON.parse(quantum.derive_external_addresses('hybrid-wallet-seed', 2, 'iuna-mainnet-v1'));
  assert.deepEqual(addresses, [
    { index: 0, address: 'iuna1py9qlrnw6cm3mpz26hwwkww9spaa96zrw2g34gu0p4y3ea5cqhg0qa82x9s' },
    { index: 1, address: 'iuna1pkxdcktlzf5tc2p59xns2nccq7rg5zjhwg2zn5r9u73c5j9jxgk8s0e5l4e' },
  ]);
  const rewardAddresses = JSON.parse(quantum.derive_reward_addresses('hybrid-wallet-seed', 2, 'iuna-mainnet-v1'));
  assert.deepEqual(rewardAddresses, [
    { index: 0, address: 'iuna1pvxpc35gavamxw0tvqj3ms82gwtvchh7mdyzqdttx7ktx7pfkgz4qkaq24k' },
    { index: 1, address: 'iuna1p6cj9c3ths75p7vnsdx9tgkvu4jlzs8cxpnh2lc4klsu5fvd0cpxq8ypw45' },
  ]);

  const externalRecoveryAddresses = JSON.parse(quantum.derive_external_addresses('hybrid-wallet-seed', 42, 'iuna-mainnet-v1'));
  const rewardRecoveryAddresses = JSON.parse(quantum.derive_reward_addresses('hybrid-wallet-seed', 40, 'iuna-mainnet-v1'));
  const recoveryTargets = [externalRecoveryAddresses[27].address, rewardRecoveryAddresses[27].address];
  const initialRecovery = core.nextRecoveryScanCounts({
    externalAddresses: externalRecoveryAddresses.slice(0, 21),
    rewardAddresses: rewardRecoveryAddresses.slice(0, 20),
    recoveryAddresses: recoveryTargets,
    gapLimit: 20,
  });
  assert.equal(initialRecovery.externalCount, 42);
  assert.equal(initialRecovery.rewardCount, 40);
  assert.deepEqual(initialRecovery.unresolved, recoveryTargets);
  const completedRecovery = core.nextRecoveryScanCounts({
    externalAddresses: externalRecoveryAddresses,
    rewardAddresses: rewardRecoveryAddresses,
    recoveryAddresses: recoveryTargets,
    gapLimit: 20,
  });
  assert.deepEqual(completedRecovery.unresolved, []);

  const built = JSON.parse(quantum.build_transfer(JSON.stringify({
    seed: 'hybrid-wallet-seed',
    chainId: 'iuna-mainnet-candidate',
    genesisHash: '11'.repeat(32),
    recipientAddress: addresses[1].address,
    amount: '1000000',
    feeRate: '1',
    changeIndex: 2,
    utxos: [{
      addressIndex: 0,
      outpoint: { txid: '22'.repeat(32), index: 7 },
      output: { amount: 2000000 },
    }],
  })));
  assert.equal(built.fee, '4079');
  assert.equal(built.envelope.length / 2, 4079);
  assert.equal(built.transactionId, 'bda748b6ba9fd6550ca47d580f980a1c00d0050a20992750265a62d23b0b590d');

  const rewardBuilt = JSON.parse(quantum.build_transfer(JSON.stringify({
    seed: 'hybrid-wallet-seed',
    chainId: 'iuna-mainnet-candidate',
    genesisHash: '11'.repeat(32),
    recipientAddress: addresses[1].address,
    amount: '1000000',
    feeRate: '1',
    changeIndex: 2,
    utxos: [{
      addressBranch: 'reward',
      addressIndex: 0,
      outpoint: { txid: '44'.repeat(32), index: 8 },
      output: { amount: 2000000 },
    }],
  })));
  assert.equal(rewardBuilt.fee, '4079');
  assert.equal(rewardBuilt.inputCount, 1);
  assert.equal(rewardBuilt.transactionId, '558cee1dfa1425e0b214681445e3d0641a898e15245f80d4524d2b98d762df88');

  const migration = JSON.parse(quantum.build_migration(JSON.stringify({
    seed: 'hybrid-wallet-seed',
    chainId: 'iuna-mainnet-candidate',
    genesisHash: '11'.repeat(32),
    destinationIndex: 2,
    feeRate: '1',
    utxos: [{
      outpoint: { txid: '33'.repeat(32), index: 4 },
      output: { amount: 2000000 },
    }],
  })));
  assert.equal(migration.fee, '307');
  assert.equal(migration.envelope.length / 2, 307);
  assert.equal(migration.transactionId, '37539c9d89a391c599b838c3f415e7adc93a36c22cd4fb5fe44a79433f336075');
});
