const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));

test('manifest declares the fixed Firefox identity and bridge permissions', () => {
  assert.equal(manifest.manifest_version, 2);
  assert.equal(manifest.version, '0.1.2');
  assert.equal(manifest.applications.gecko.id, 'curl-downloader@kinkeil.local');
  assert.ok(manifest.permissions.includes('downloads'));
  assert.ok(manifest.permissions.includes('nativeMessaging'));
  assert.ok(manifest.permissions.includes('storage'));
  assert.equal(manifest.background.persistent, true);
  assert.ok(manifest.background.scripts.indexOf('core.js') < manifest.background.scripts.indexOf('background.js'));
});

test('settings exposes native host retry control', () => {
  const settingsHtml = fs.readFileSync(path.join(root, 'settings.html'), 'utf8');
  const settingsJs = fs.readFileSync(path.join(root, 'settings.js'), 'utf8');
  const settingsCss = fs.readFileSync(path.join(root, 'settings.css'), 'utf8');
  assert.match(settingsHtml, /id="retry-native"/);
  assert.match(settingsHtml, /重試 Curl Downloader/);
  assert.match(settingsJs, /get-defaults/);
  assert.match(settingsJs, /retry-native/);
  assert.match(settingsJs, /cancel-download/);
  assert.match(settingsCss, /retry-native/);
});
test('documents GUI startup native host registration', () => {
  const readme = fs.readFileSync(path.join(root, '..', 'README.md'), 'utf8');
  const portableScript = fs.readFileSync(path.join(root, '..', 'scripts', 'package-portable.ps1'), 'utf8');
  assert.match(readme, /GUI 啟動時會自動在 `HKCU/);
  assert.match(readme, /重試 Curl Downloader/);
  assert.match(portableScript, /GUI 啟動時會自動註冊 Firefox Native host/);
});

test('storage implementation has no password persistence path', () => {
  const source = fs.readFileSync(path.join(root, 'storage.js'), 'utf8');
  assert.equal(source.includes('password'), false);
});

test('native host installation and XPI packaging scripts are present', () => {
  const installScript = fs.readFileSync(path.join(root, '..', 'scripts', 'install-firefox-native-host.ps1'), 'utf8');
  const packageScript = fs.readFileSync(path.join(root, '..', 'scripts', 'package-firefox-extension.ps1'), 'utf8');
  assert.match(installScript, /curl_downloader/);
  assert.match(installScript, /curl-downloader@kinkeil\.local/);
  assert.match(installScript, /allowed_extensions/);
  assert.match(packageScript, /Compress-Archive/);
});
