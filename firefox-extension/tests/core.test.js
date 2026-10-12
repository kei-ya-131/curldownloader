const test = require('node:test');
const assert = require('node:assert/strict');
const core = require('../core.js');

test('native preflight accepts a large cookie without truncation and rejects excessive totals', () => {
  const cookie = `session=${'x'.repeat(40 * 1024)}`;
  const wire = core.serializeRequestContext({ headers: [{ name: 'Cookie', value: cookie }] },
    { url: 'https://files.test/file' });
  assert.equal(core.requestContextDiagnostic(wire), null);
  assert.equal(wire.headers[0].value, cookie);
  wire.headers.push({ name: 'Authorization', value: 'x'.repeat(10 * 1024) });
  assert.equal(core.requestContextDiagnostic(wire), 'request context is too large');
});

test('request context diagnostics identify limits without returning credentials', () => {
  const wire = core.serializeRequestContext({ headers: [] }, { url: 'https://files.test/file' });
  assert.equal(core.requestContextDiagnostic(wire), null);
  wire.headers = [{ name: 'Cookie', value: 'x'.repeat(48 * 1024 + 1) }];
  assert.equal(core.requestContextDiagnostic(wire), 'header value is too large');
  wire.headers = [{ name: 'Authorization', value: 'fictional-secret\r\n' }];
  assert.equal(core.requestContextDiagnostic(wire), 'header value contains invalid characters');
  wire.headers = [];
  wire.source_page_url = 'blob:https://files.test/fictional';
  assert.equal(core.requestContextDiagnostic(wire), 'source page URL must be an HTTP or HTTPS URL');
  assert.equal(core.nativeContextDiagnostic({ code: 'invalid_request_context',
    message: 'fictional-secret; header value is too large' }), 'header value is too large');
  assert.equal(core.nativeContextDiagnostic({ code: 'invalid_request_context',
    message: 'fictional-secret' }), null);
  assert.equal(core.nativeContextDiagnostic({ code: 'invalid_proxy',
    message: 'header value is too large' }), null);
});

test('missing cookie store stays null across the native messaging boundary', () => {
  for (const cookieStoreId of [undefined, null, '']) {
    const wire = core.serializeRequestContext({
      headers: [{ name: 'Authorization', value: 'Bearer fictional' }],
      cookieStoreId
    }, { url: 'https://files.test/file' });
    assert.equal(wire.cookie_store_id, null);
    assert.equal(wire.headers[0].value, 'Bearer fictional');
  }
});

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
      segments: 6,
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
  assert.equal(message.requested_segments, 6);
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

test('enqueue message carries the Firefox request context separately from proxy settings', () => {
  const context = {
    headers: [{ name: 'Cookie', value: 'session=secret' }],
    sourcePageUrl: 'https://app.test/page',
    initialUrl: 'https://files.test/a.pdf',
    finalUrl: 'https://files.test/a.pdf',
    incognito: true,
    cookieStoreId: 'firefox-container-2'
  };
  const message = core.buildEnqueueMessage(
    { url: context.initialUrl, filename: 'a.pdf' },
    { filename: 'a.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false } },
    'request-context-1',
    context
  );
  assert.deepEqual(message.request_context, {
    headers: context.headers,
    source_page_url: context.sourcePageUrl,
    initial_url: context.initialUrl,
    final_url: context.finalUrl,
    incognito: context.incognito,
    cookie_store_id: context.cookieStoreId
  });
  assert.equal(message.proxy.password, '');
});
