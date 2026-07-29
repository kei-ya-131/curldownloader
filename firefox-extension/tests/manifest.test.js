const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'manifest.json'), 'utf8'));

test('manifest declares the fixed Firefox identity and bridge permissions', () => {
  assert.equal(manifest.manifest_version, 2);
  assert.equal(manifest.version, '0.1.4');
  assert.equal(manifest.applications.gecko.id, 'curl-downloader@kinkeil.local');
  assert.ok(manifest.permissions.includes('downloads'));
  assert.ok(manifest.permissions.includes('nativeMessaging'));
  assert.ok(manifest.permissions.includes('storage'));
  assert.equal(manifest.background.persistent, true);
  assert.equal(manifest.browser_action.default_popup, 'popup.html');
  assert.deepEqual(manifest.background.scripts, ['core.js', 'storage.js', 'status.js', 'native-session.js', 'background.js']);
  assert.ok(manifest.background.scripts.indexOf('core.js') < manifest.background.scripts.indexOf('background.js'));
});

test('declares cyber add-on and toolbar icons', () => {
  const cyberIcons = {
    16: 'icons/curl-downloader-16.png',
    32: 'icons/curl-downloader-32.png',
    48: 'icons/curl-downloader-48.png'
  };
  assert.deepEqual(manifest.icons, cyberIcons);
  assert.deepEqual(manifest.browser_action.default_icon, cyberIcons);
  for (const relativePath of Object.values(cyberIcons)) {
    assert.equal(fs.existsSync(path.join(root, relativePath)), true);
  }
});
test('declares cyber toolbar and progress icons', () => {
  assert.deepEqual(manifest.browser_action.default_icon, {
    16: 'icons/curl-downloader-16.png',
    32: 'icons/curl-downloader-32.png',
    48: 'icons/curl-downloader-48.png'
  });
  assert.ok(manifest.background.scripts.includes('status.js'));
  for (const size of [16, 32, 48]) {
    assert.equal(fs.existsSync(path.join(root, 'icons', `curl-downloader-${size}.png`)), true);
  }
  for (let percent = 0; percent <= 100; percent += 10) {
    assert.equal(fs.existsSync(path.join(root, 'icons', `progress-${String(percent).padStart(3, '0')}.png`)), true);
  }
});

test('release scripts reference one GUI executable and extension assets', () => {
  const scriptsRoot = path.join(root, '..', 'scripts');
  const release = fs.readFileSync(path.join(scriptsRoot, 'build-release.ps1'), 'utf8');
  const gnuRelease = fs.readFileSync(path.join(scriptsRoot, 'build-release-gnu.ps1'), 'utf8');
  const portable = fs.readFileSync(path.join(scriptsRoot, 'package-portable.ps1'), 'utf8');
  const packageExtension = fs.readFileSync(path.join(scriptsRoot, 'package-firefox-extension.ps1'), 'utf8');
  for (const source of [release, gnuRelease, portable, packageExtension]) {
    assert.equal(source.includes('CurlDownloader-native-host.exe'), false);
  }
  assert.match(packageExtension, /status\.js/);
  assert.match(packageExtension, /icons/);
  assert.match(portable, /exactly one|單一|一份/i);
  assert.match(portable, /portable\.flag/);
  assert.doesNotMatch(portable, /Install-Firefox-Native-Host\.ps1/);
  assert.doesNotMatch(portable, /Start-CurlDownloader-Portable\.ps1/);
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
  assert.match(installScript, /CurlDownloader\.exe/);
  assert.doesNotMatch(installScript, /nativeHostSuffix|native-host\.exe/);
  assert.match(packageScript, /IO\.Compression\.ZipArchive/);
  assert.match(packageScript, /'popup\.html'/);
  assert.match(packageScript, /'popup\.js'/);
});
