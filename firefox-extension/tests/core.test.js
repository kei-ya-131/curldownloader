const test = require('node:test');
const assert = require('node:assert/strict');
const core = require('../core.js');

test('only http and https downloads enter Curl Downloader flow', () => {
  assert.equal(core.isSupportedDownloadUrl('https://example.test/a.zip'), true);
  assert.equal(core.isSupportedDownloadUrl('http://example.test/a.zip'), true);
  assert.equal(core.isSupportedDownloadUrl('blob:https://example.test/id'), false);
  assert.equal(core.isSupportedDownloadUrl('file:///C:/a.zip'), false);
});

test('enqueue message serializes the complete proxy form', () => {
  const message = core.buildEnqueueMessage(
    { url: 'https://example.test/a.zip', filename: 'a.zip' },
    {
      filename: 'renamed.zip',
      targetDir: 'C:\\Downloads',
      proxy: {
        enabled: true, protocol: 'socks5h', host: '127.0.0.1',
        port: '1080', username: 'alice', password: 'secret'
      }
    },
    'request-1'
  );
  assert.equal(message.type, 'enqueue');
  assert.equal(message.filename, 'renamed.zip');
  assert.equal(message.target_dir, 'C:\\Downloads');
  assert.equal(message.url, 'https://example.test/a.zip');
  assert.equal(message.proxy.protocol, 'socks5h');
  assert.equal(message.proxy.port, 1080);
  assert.equal(message.proxy.password, 'secret');
  assert.equal(Object.keys(message.proxy).sort().join(','), 'enabled,host,password,port,protocol,username');
});

test('fallback filename is a relative basename', () => {
  assert.equal(core.fallbackFilename('C:\\Downloads\\renamed.zip'), 'renamed.zip');
  assert.equal(core.fallbackFilename(''), 'download.bin');
});
