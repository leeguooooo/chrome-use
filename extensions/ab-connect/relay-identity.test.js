import assert from 'node:assert/strict';
import test from 'node:test';
import { extensionInstallIdentity } from './relay-identity.js';
import { readFileSync } from 'node:fs';

function api(manifest = {}, management) {
  return { runtime: { id: 'test-extension', getManifest: () => manifest }, management };
}

test('hello identity reports actual extension id and management install type', async () => {
  assert.deepEqual(await extensionInstallIdentity(api({}, { getSelf: async () => ({ installType: 'normal' }) }), 'Chrome/154'),
    { extensionId: 'test-extension', installType: 'normal', browser: 'chrome' });
});

test('install identity fallback is conservative when management is unavailable', async () => {
  assert.equal((await extensionInstallIdentity(api())).installType, 'development');
  assert.equal((await extensionInstallIdentity(api({ update_url: 'https://example.test' }))).installType, 'unknown');
  assert.equal((await extensionInstallIdentity(api({}, { getSelf: async () => { throw new Error('denied'); } }))).installType, 'development');
  assert.equal((await extensionInstallIdentity(api(), 'Chrome/154 Edg/154')).browser, 'edge');
  assert.equal((await extensionInstallIdentity(api(), '')).browser, null);
});

test('health replies and hello wire the added helpers', () => {
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  assert.match(source, /const extra = await extensionInstallIdentity\(chrome\)/);
  assert.equal(source.split('relayHealth: getRelayHealth()').length - 1, 2);
});
