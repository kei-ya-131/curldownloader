const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));

test('manifest declares the fixed Firefox identity and bridge permissions', () => {
  assert.equal(manifest.manifest_version, 2);
  assert.equal(manifest.applications.gecko.id, 'curl-downloader@kinkeil.local');
  assert.ok(manifest.permissions.includes('downloads'));
  assert.ok(manifest.permissions.includes('nativeMessaging'));
  assert.ok(manifest.permissions.includes('storage'));
  assert.equal(manifest.background.persistent, true);
  assert.ok(manifest.background.scripts.indexOf('core.js') < manifest.background.scripts.indexOf('background.js'));
});

test('storage implementation has no password persistence path', () => {
  const source = fs.readFileSync(path.join(root, 'storage.js'), 'utf8');
  assert.equal(source.includes('password'), false);
});
