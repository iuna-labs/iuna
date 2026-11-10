const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');

const root = join(__dirname, '..');
const source = (path) => readFileSync(join(root, path), 'utf8');
const dataModule = (text) => `data:text/javascript;base64,${Buffer.from(text).toString('base64')}`;

let core;
let mnemonic;

test.before(async () => {
  const coreUrl = dataModule(source('wallet/wallet-core.js'));
  core = await import(coreUrl);
  const wordsUrl = dataModule(source('wallet/words.js'));
  const mnemonicSource = source('wallet/mnemonic.js')
    .replace('./words.js', wordsUrl)
    .replace('./wallet-core.js', coreUrl);
  mnemonic = await import(dataModule(mnemonicSource));
});

test('wallet uses canonical entropy and commits its Ed25519 public key', async () => {
  const phrase = `${Array(23).fill('abandon').join(' ')} art`;
  const entropy = await mnemonic.mnemonicEntropy(phrase);
  const wallet = await core.walletFromEntropy(entropy);

  assert.equal(entropy.length, 32);
  assert.equal(wallet.publicKeyHex, '3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29');
  assert.equal(wallet.addressHex.length, 64);
  assert.notEqual(wallet.addressHex, wallet.publicKeyHex);
  assert.equal(await mnemonic.validateMnemonic(phrase), phrase);
  await assert.rejects(mnemonic.validateMnemonic(`${Array(24).fill('abandon').join(' ')}`), /checksum/);
});

test('browser wallet builds signatures accepted by Ed25519', async () => {
  const wallet = await core.walletFromEntropy(new Uint8Array(32));
  const transaction = await core.buildSignedTransaction({
    wallet,
    utxos: [{ transaction_id: '11'.repeat(32), output_index: 3, value: 1000, reserved: false }],
    recipient: '22'.repeat(32),
    amount: 600,
    fee: 10,
  });

  assert.equal(transaction.version, 3);
  assert.deepEqual(transaction.outputs.map((output) => output.value), [600, 390]);
  assert.notEqual(core.bytesHex(transaction.outputs[1].owner), wallet.addressHex);
  assert.equal(transaction.inputs[0].signature.length, 64);
  const publicKey = await crypto.subtle.importKey('raw', wallet.publicKey, 'Ed25519', false, ['verify']);
  assert.equal(
    await crypto.subtle.verify('Ed25519', publicKey, new Uint8Array(transaction.inputs[0].signature), await core.signingDigest(transaction, 0, [1000])),
    true,
  );

  const next = await core.buildSignedTransaction({
    wallet,
    utxos: [{ transaction_id: '11'.repeat(32), output_index: 3, value: 1000, reserved: false }],
    recipient: '22'.repeat(32),
    amount: 600,
    fee: 10,
    changeIndex: 1,
  });
  assert.notDeepEqual(next.outputs[1].owner, transaction.outputs[1].owner);
});

test('browser wallet loads every stable UTXO page before selecting inputs', async () => {
  const originalFetch = globalThis.fetch;
  const requests = [];
  globalThis.fetch = async (url) => {
    requests.push(String(url));
    const second = String(url).includes('utxo_offset=1000');
    return new Response(JSON.stringify({
      network: 'iuna-mainnet-candidate',
      heartbeat_height: 7,
      tip_hash: 'ff'.repeat(32),
      ledger_blocks: 1,
      balance: 3,
      spendable_balance: 3,
      utxos: [{ transaction_id: (second ? '22' : '11').repeat(32), output_index: 0, value: second ? 2 : 1, reserved: false }],
      utxos_truncated: !second,
      next_utxo_offset: second ? null : 1000,
    }), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  try {
    const view = await core.fetchWalletView('https://wallet.getiuna.com', 'aa'.repeat(32));
    assert.equal(view.utxos.length, 2);
    assert.equal(view.utxos_truncated, false);
    assert.equal(view.next_utxo_offset, null);
    assert.equal(requests.length, 2);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test('browser wallet discovers the next receive and change addresses', async () => {
  const wallet = await core.walletFromEntropy(new Uint8Array(32));
  const receive0 = await wallet.keyAt(0, 0);
  const receive1 = await wallet.keyAt(0, 1);
  const change0 = await wallet.keyAt(1, 0);
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url) => {
    const address = String(url).split('/').pop().split('?')[0];
    return new Response(JSON.stringify({
      network: 'iuna-mainnet-candidate',
      heartbeat_height: 7,
      tip_hash: 'ff'.repeat(32),
      ledger_blocks: 1,
      address,
      used: [receive0.addressHex, change0.addressHex].includes(address),
      balance: 0,
      spendable_balance: 0,
      utxos: [],
      pending_transaction_ids: [],
      utxos_truncated: false,
      next_utxo_offset: null,
    }), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  try {
    const view = await core.fetchWalletView('https://wallet.getiuna.com', wallet);
    assert.equal(view.receive_index, 1);
    assert.equal(view.receive_address, receive1.addressHex);
    assert.equal(view.change_index, 1);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test('sent activity separates pending, mined and dropped transactions', async () => {
  const wallet = await core.walletFromEntropy(new Uint8Array(32));
  const transaction = await core.buildSignedTransaction({
    wallet,
    utxos: [{ transaction_id: '11'.repeat(32), output_index: 3, value: 1000, reserved: false }],
    recipient: '22'.repeat(32),
    amount: 600,
    fee: 1,
  });
  const inputs = core.spentOutpoints(transaction);
  assert.deepEqual(inputs, [`${'11'.repeat(32)}:3`]);

  const item = { id: 'tx', inputs };
  const unspent = [{ transaction_id: '11'.repeat(32), output_index: 3 }];
  const view = (fields) => ({ tip_hash: 'tip', utxos: [], pending_transaction_ids: [], ...fields });
  assert.equal(core.sentStatus(item, view({ utxos: unspent, pending_transaction_ids: ['tx'] })), 'pending');
  assert.equal(core.sentStatus(item, view({ utxos: unspent })), 'dropped');
  assert.equal(core.sentStatus(item, view()), 'sent');
  // Without a node there is no evidence either way.
  assert.equal(core.sentStatus(item, { utxos: [] }), 'pending');
});

test('wallet settings name reachable endpoints and previous wallets are explained', () => {
  const app = source('wallet/app.js');
  for (const key of ['iuna.wallets.v2', 'iuna.wallet.v1']) assert.ok(app.includes(`"${key}"`));
  assert.match(app, /previousNetworkNotice\(\)/);
  assert.doesNotMatch(app, /127\.0\.0\.1:8080/);
  for (const policy of source('nginx.conf').match(/connect-src [^;]+/g)) {
    assert.ok(policy.includes('http://127.0.0.1:18662'));
  }
  assert.match(app, /data-field="receive-address"/);
  assert.match(app, /copy-receive.*dataset\.value = view\.receive_address/s);
  assert.match(app, /submitTransaction\(apiEndpoint\(\), transaction\);[^]*?view\.change_index = \(view\.change_index \|\| 0\) \+ 1/);
});

test('wallet app and deployment contain only the current protocol identity', () => {
  const combined = [
    source('wallet/index.html'),
    source('wallet/app.js'),
    source('wallet/wallet-core.js'),
  ].join('\n');
  assert.match(combined, /Heartbeat/);
  assert.match(combined, /Ledger/);
  assert.match(combined, /Ed25519/);
  assert.doesNotMatch(combined, /quantum|VDF|L1|L2/i);
  assert.match(source('config/deployment.yml'), /wallet\.getiuna\.com/);
  assert.match(source('Dockerfile'), /COPY wallet \/site\/wallet/);
  assert.doesNotMatch(source('wallet/index.html'), /https?:\/\//);
  assert.match(source('scripts/inject_cloudflare_analytics.py'), /is_relative_to\(wallet_root\)/);
  assert.match(source('nginx.conf'), /Content-Security-Policy[^\n]+script-src 'self'/);
});

test('browser wallet signs for the pinned production genesis', () => {
  const [, documented] = source('docs/protocol.md').match(/genesis hash: `([0-9a-f]{64})`/);
  assert.ok(source('wallet/wallet-core.js').includes(`hexBytes("${documented}")`));
  assert.equal(JSON.parse(source('tests/vectors/browser_wallet_transaction.json')).genesis_hash, documented);
  const pinned = source('src/consensus.rs').match(/#\[cfg\(not\(any\(test, feature = "e2e"\)\)\)\]\npub const GENESIS_HASH: Hash = \[([^\]]+)\]/);
  const bytes = pinned[1].match(/0x[0-9a-f]{2}/g).map((byte) => byte.slice(2)).join('');
  assert.equal(bytes, documented);
});

test('browser wallet reproduces the vector verified by the Rust validator', async () => {
  const vector = JSON.parse(source('tests/vectors/browser_wallet_transaction.json'));
  const wallet = await core.walletFromEntropy(core.hexBytes(vector.seed_hex));
  const transaction = await core.buildSignedTransaction({
    wallet,
    utxos: [
      { transaction_id: '11'.repeat(32), output_index: 3, value: 1000, reserved: false },
      { transaction_id: '33'.repeat(32), output_index: 0, value: 500, reserved: false },
    ],
    recipient: '22'.repeat(32),
    amount: 1200,
    fee: 10,
  });

  assert.equal(wallet.publicKeyHex, vector.public_key_hex);
  assert.equal(wallet.privateKey.extractable, false);
  assert.deepEqual(transaction, vector.transaction);
  for (const [index, digest] of vector.signing_digests.entries()) {
    assert.equal(core.bytesHex(await core.signingDigest(transaction, index, vector.input_values)), digest);
  }

  const lied = await core.signingDigest(transaction, 0, [10, 500]);
  assert.notEqual(core.bytesHex(lied), vector.signing_digests[0]);
});
