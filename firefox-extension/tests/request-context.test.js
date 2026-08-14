const test = require('node:test');
const assert = require('node:assert/strict');
const {
  createRequestContextTracker,
  filterRequestHeaders
} = require('../request-context.js');

test('filters transport-owned headers and keeps website identity', () => {
  assert.deepEqual(filterRequestHeaders([
    { name: 'Host', value: 'files.example.test' },
    { name: 'Cookie', value: 'session=abc' },
    { name: 'Referer', value: 'https://example.test/page' },
    { name: 'Range', value: 'bytes=0-1' },
    { name: 'Accept-Encoding', value: 'gzip, br' }
  ]), [
    { name: 'Cookie', value: 'session=abc' },
    { name: 'Referer', value: 'https://example.test/page' }
  ]);
});

test('rejects request headers containing a newline', () => {
  assert.throws(
    () => filterRequestHeaders([{ name: 'X-Test', value: 'ok\r\nInjected: yes' }]),
    /invalid request header/i
  );
});

test('claims the exact redirect chain for a Firefox DownloadItem', () => {
  let now = 1_000;
  const tracker = createRequestContextTracker({ now: () => now, ttlMs: 15_000, maxEntries: 256 });
  tracker.observeSendHeaders({
    requestId: 'r1', method: 'GET', url: 'https://example.test/start', tabId: 4,
    requestHeaders: [{ name: 'Cookie', value: 'session=one' }]
  });
  tracker.observeRedirect({
    requestId: 'r1', url: 'https://example.test/start', redirectUrl: 'https://cdn.test/file.zip'
  });
  tracker.observeSendHeaders({
    requestId: 'r1', method: 'GET', url: 'https://cdn.test/file.zip', tabId: 4,
    requestHeaders: [{ name: 'Cookie', value: 'cdn=two' }]
  });
  assert.deepEqual(tracker.claimDownload({
    id: 9, url: 'https://example.test/start', referrer: 'https://example.test/page',
    incognito: false, cookieStoreId: 'firefox-default'
  }), {
    headers: [{ name: 'Cookie', value: 'cdn=two' }],
    sourcePageUrl: 'https://example.test/page',
    initialUrl: 'https://example.test/start',
    finalUrl: 'https://cdn.test/file.zip',
    tabId: 4,
    incognito: false,
    cookieStoreId: 'firefox-default',
    capturedUnixMs: 1_000
  });
});

test('does not swap same-url credentials between tabs and consumes claims once', () => {
  const tracker = createRequestContextTracker({ now: () => 2_000 });
  tracker.observeSendHeaders({
    requestId: 'tab-a', method: 'GET', url: 'https://files.test/a.zip', tabId: 10,
    requestHeaders: [{ name: 'Cookie', value: 'a=1' }]
  });
  tracker.observeSendHeaders({
    requestId: 'tab-b', method: 'GET', url: 'https://files.test/a.zip', tabId: 11,
    requestHeaders: [{ name: 'Cookie', value: 'b=2' }]
  });
  const claimed = tracker.claimDownload({
    id: 1, url: 'https://files.test/a.zip', referrer: 'https://app.test/a',
    tabId: 11, incognito: false, cookieStoreId: 'firefox-default'
  });
  assert.equal(claimed, null);
});

test('returns the closest unique same-tab request and does not claim it twice', () => {
  let now = 3_000;
  const tracker = createRequestContextTracker({ now: () => now });
  tracker.observeSendHeaders({
    requestId: 'r1', method: 'GET', url: 'https://files.test/a.zip', tabId: 10,
    requestHeaders: [{ name: 'Cookie', value: 'a=1' }]
  });
  const download = {
    id: 1, url: 'https://files.test/a.zip', referrer: 'https://app.test/a',
    tabId: 10, incognito: false, cookieStoreId: 'firefox-default'
  };
  const claimed = tracker.claimDownload(download);
  assert.equal(claimed.headers[0].value, 'a=1');
  assert.equal(tracker.claimDownload(download), null);
  now += 15_001;
  tracker.observeSendHeaders({
    requestId: 'expired', method: 'GET', url: 'https://files.test/expired.zip', tabId: 10,
    requestHeaders: [{ name: 'Cookie', value: 'expired=1' }]
  });
  now += 15_001;
  assert.equal(tracker.claimDownload({
    id: 2, url: 'https://files.test/expired.zip', referrer: 'https://app.test/a',
    tabId: 10, incognito: false, cookieStoreId: 'firefox-default'
  }), null);
});

test('discards non-GET observations and carries private/container metadata', () => {
  const tracker = createRequestContextTracker({ now: () => 4_000 });
  tracker.observeSendHeaders({
    requestId: 'post', method: 'POST', url: 'https://files.test/a.zip', tabId: 10,
    requestHeaders: [{ name: 'Cookie', value: 'post=1' }]
  });
  assert.equal(tracker.claimDownload({
    id: 1, url: 'https://files.test/a.zip', referrer: 'https://app.test/a',
    tabId: 10, incognito: true, cookieStoreId: 'firefox-container-2'
  }), null);

  tracker.observeSendHeaders({
    requestId: 'private', method: 'GET', url: 'https://files.test/a.zip', tabId: 10,
    requestHeaders: [{ name: 'Cookie', value: 'private=1' }]
  });
  const claimed = tracker.claimDownload({
    id: 2, url: 'https://files.test/a.zip', referrer: 'https://app.test/a',
    tabId: 10, incognito: true, cookieStoreId: 'firefox-container-2'
  });
  assert.equal(claimed.incognito, true);
  assert.equal(claimed.cookieStoreId, 'firefox-container-2');
});

test('rejects overlarge values and caps the number of retained entries', () => {
  assert.throws(
    () => filterRequestHeaders([{ name: 'X-Test', value: 'x'.repeat(8 * 1024 + 1) }]),
    /too large/i
  );
  const tracker = createRequestContextTracker({ now: () => 5_000, maxEntries: 2 });
  for (const id of ['one', 'two', 'three']) {
    tracker.observeSendHeaders({
      requestId: id, method: 'GET', url: `https://files.test/${id}.zip`, tabId: 1,
      requestHeaders: [{ name: 'Cookie', value: `${id}=1` }]
    });
  }
  assert.equal(tracker.claimDownload({
    id: 1, url: 'https://files.test/one.zip', referrer: 'https://app.test/a',
    tabId: 1, incognito: false, cookieStoreId: 'firefox-default'
  }), null);
});

test('reauthorization captures one fresh same-tab resource request only', () => {
  let now = 6_000;
  const tracker = createRequestContextTracker({ now: () => now });
  const session = tracker.beginReauthorization({
    tabId: 7,
    sourcePageUrl: 'https://app.test/chat?view=1',
    initialUrl: 'https://files.test/download?sig=old',
    finalUrl: 'https://cdn.test/file.pdf?sig=old',
    incognito: true,
    cookieStoreId: 'firefox-container-1'
  });
  tracker.observeSendHeaders({
    requestId: 'wrong-tab', method: 'GET', url: 'https://files.test/download?sig=new', tabId: 8,
    documentUrl: 'https://app.test/chat',
    requestHeaders: [{ name: 'Cookie', value: 'wrong=1' }]
  });
  tracker.observeSendHeaders({
    requestId: 'fresh', method: 'GET', url: 'https://files.test/download?sig=new', tabId: 7,
    documentUrl: 'https://app.test/chat',
    requestHeaders: [{ name: 'Cookie', value: 'fresh=1' }]
  });
  assert.deepEqual(tracker.claimReauthorization(session), {
    headers: [{ name: 'Cookie', value: 'fresh=1' }],
    sourcePageUrl: 'https://app.test/chat?view=1',
    initialUrl: 'https://files.test/download?sig=new',
    finalUrl: 'https://files.test/download?sig=new',
    tabId: 7,
    incognito: true,
    cookieStoreId: 'firefox-container-1',
    capturedUnixMs: 6_000
  });
  assert.equal(tracker.claimReauthorization(session), null);
  now += 1;
});
