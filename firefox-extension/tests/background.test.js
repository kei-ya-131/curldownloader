const test = require('node:test');
const assert = require('node:assert/strict');
const createBackground = require('../background.js');

const pageSender = { url: 'https://chatgpt.com/c/example', frameId: 0,
  tab: { id: 7, incognito: false, cookieStoreId: 'firefox-default' } };

test('page blob download opens settings before Firefox writes and keeps exact GET credentials', async () => {
  const fake = makeFakeBrowser({ nativeResponse: (m) => ({
    type: m.type === 'enqueue' ? 'enqueue_result' : 'task_list', ok: true,
    request_id: m.request_id, task_id: 42, tasks: [], awaiting_file_decision: false
  }) });
  const background = createBackground(fake.browser, { timers: false });
  fake.events.sendHeaders({ requestId: 'file', method: 'GET', tabId: 7, frameId: 0,
    url: 'https://files.example.test/signed?token=fictional', documentUrl: pageSender.url,
    requestHeaders: [{ name: 'Authorization', value: 'Bearer fictional' }] });
  const result = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url: 'https://files.example.test/signed?token=fictional', filename: 'report.pdf', fallbackToken: 'test-1' }, pageSender);
  assert.equal(result.ok, true);
  assert.equal(fake.calls.tabs.length, 1);
  assert.deepEqual(fake.calls.pause, []);
  const submitted = await background.submitExternalDownload(result.downloadId, {
    filename: 'report.pdf', targetDir: 'C:\\Downloads', segments: 4, proxy: { enabled: false }
  });
  assert.equal(submitted.ok, true);
  const enqueue = fake.calls.nativeMessages.find((m) => m.type === 'enqueue');
  assert.equal(enqueue.request_context.headers[0].value, 'Bearer fictional');
  assert.equal(enqueue.request_context.cookie_store_id, 'firefox-default');
  assert.equal(fake.calls.pageMessages.at(-1).message.type, 'release-page-download');
  assert.deepEqual(fake.calls.cancel, []);
});

test('unmapped blob still offers Firefox fallback without submitting a guessed HTTP URL', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { timers: false });
  const result = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url: null, filename: 'report.pdf', fallbackToken: 'test-2' }, pageSender);
  assert.equal(result.ok, true);
  const view = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: result.downloadId });
  assert.equal(view.download.externalSupported, false);
  const submitted = await background.submitExternalDownload(result.downloadId, {});
  assert.equal(submitted.ok, false);
  assert.equal(fake.calls.nativeMessages.length, 0);
  const restored = await background.handleRuntimeMessage({ type: 'restore-firefox', downloadId: result.downloadId });
  assert.equal(restored.ok, true);
  assert.equal(fake.calls.pageMessages[0].message.fallbackToken, 'test-2');
  assert.equal(fake.calls.pageMessages[0].message.type, 'restore-page-download');
});

test('page interception rejects other origins and does not claim a different tab request', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { timers: false });
  const message = { type: 'intercept-page-download', url: 'https://files.example.test/file',
    filename: 'report.pdf', fallbackToken: 'test-3' };
  assert.equal((await background.handleRuntimeMessage(message, { ...pageSender, url: 'https://other.test/' })).ok, false);
  fake.events.sendHeaders({ requestId: 'wrong-tab', method: 'GET', tabId: 8,
    url: message.url, documentUrl: pageSender.url,
    requestHeaders: [{ name: 'Authorization', value: 'Bearer wrong-tab' }] });
  const result = await background.handleRuntimeMessage(message, pageSender);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: result.downloadId });
  assert.equal(pending.download.externalSupported, false);
});

test('closing blob settings restores its source frame once; cancelling releases without a download', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { timers: false });
  const message = { type: 'intercept-page-download', url: null,
    filename: 'report.pdf', fallbackToken: 'close-test' };
  const result = await background.handleRuntimeMessage(message, pageSender);
  assert.equal((await background.handleRuntimeMessage(message, pageSender)).downloadId, result.downloadId);
  assert.equal(fake.calls.tabs.length, 1);
  fake.events.removed(fake.calls.tabs[0].id);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(fake.calls.pageMessages[0], { id: 7,
    message: { type: 'restore-page-download', fallbackToken: 'close-test' }, options: { frameId: 0 } });
  assert.equal((await background.handleRuntimeMessage({ type: 'get-pending', downloadId: result.downloadId })).ok, false);
  const other = await background.handleRuntimeMessage({ ...message, fallbackToken: 'cancel-test' }, pageSender);
  const cancelled = await background.handleRuntimeMessage({ type: 'cancel-download', downloadId: other.downloadId });
  assert.equal(cancelled.ok, true);
  assert.equal(fake.calls.pageMessages.at(-1).message.type, 'release-page-download');
  assert.deepEqual(fake.calls.download, []);
});

test('blob fallback failure keeps settings retry state instead of losing the file', async () => {
  const fake = makeFakeBrowser();
  fake.browser.tabs.sendMessage = async () => { throw new Error('frame navigated'); };
  const background = createBackground(fake.browser, { timers: false });
  const result = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url: null, filename: 'report.pdf', fallbackToken: 'failed-fallback' }, pageSender);
  assert.equal((await background.handleRuntimeMessage({ type: 'restore-firefox', downloadId: result.downloadId })).ok, false);
  assert.equal((await background.handleRuntimeMessage({ type: 'get-pending', downloadId: result.downloadId })).ok, true);
});

test('blob interception waits for Firefox delayed webRequest headers before deciding support', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { timers: false, pageCaptureWaitMs: 100 });
  const url = 'https://files.example.test/fast-file';
  setTimeout(() => fake.events.sendHeaders({ requestId: 'delayed', method: 'GET',
    url, tabId: 7, frameId: 0, documentUrl: pageSender.url,
    requestHeaders: [{ name: 'Authorization', value: 'Bearer fictional-delayed' }] }), 10);
  const result = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url, filename: 'report.pdf', fallbackToken: 'delayed' }, pageSender);
  assert.equal((await background.handleRuntimeMessage({ type: 'get-pending', downloadId: result.downloadId })).download.externalSupported, true);
});

function makeFakeBrowser({
  resumeFails = false,
  pauseFails = false,
  eraseFails = false,
  downloadFails = false,
  tabCreateFails = false,
  nativeDisconnect = false,
  nativeFailuresBeforeSuccess = 0,
  nativeDelayMs = 0,
  nativeResponse = null,
  sourceTabs = [],
  cookieResults = []
} = {}) {
  const events = {
    created: null,
    removed: null,
    message: null,
    sendHeaders: null,
    redirect: null,
    completed: null,
    errorOccurred: null
  };
  const calls = {
    pageMessages: [],
    pause: [],
    resume: [],
    cancel: [],
    erase: [],
    download: [],
    tabs: [],
    tabUpdates: [],
    notifications: [],
    webRequest: [],
    nativeMessages: [],
    cookieQueries: [],
    nativeConnects: 0,
    nativeDisconnects: 0,
    badgeText: [],
    badgeColor: [],
    badgeTitles: [],
    badgeIcons: []
  };
  let nextTabId = 10;
  let nextDownloadId = 100;
  const browser = {
    downloads: {
      onCreated: { addListener(listener) { events.created = listener; } },
      pause: async (id) => {
        calls.pause.push(id);
        if (pauseFails) throw new Error('pause failed');
      },
      resume: async (id) => {
        calls.resume.push(id);
        if (resumeFails) throw new Error('resume failed');
      },
      cancel: async (id) => { calls.cancel.push(id); },
      erase: async (query) => {
        calls.erase.push(query);
        if (eraseFails) throw new Error('erase failed');
      },
      download: async (details) => {
        calls.download.push(details);
        if (downloadFails) throw new Error('download failed');
        const id = nextDownloadId++;
        queueMicrotask(() => events.created && events.created({
          id,
          url: details.url,
          filename: details.filename
        }));
        return id;
      }
    },
    tabs: {
      sendMessage: async (id, message, options) => {
        calls.pageMessages.push({ id, message, options });
        return { ok: true };
      },
      onRemoved: { addListener(listener) { events.removed = listener; } },
      create: async (details) => {
        if (tabCreateFails) throw new Error('tab create failed');
        const tab = { id: nextTabId++, ...details };
        calls.tabs.push(tab);
        return tab;
      },
      get: async (id) => {
        const tab = sourceTabs.find((candidate) => candidate.id === id);
        if (!tab) throw new Error('tab not found');
        return tab;
      },
      update: async (id, details) => {
        calls.tabUpdates.push({ id, details });
        return { id, ...details };
      },
      query: async () => sourceTabs
    },
    webRequest: {
      onSendHeaders: { addListener(listener) { events.sendHeaders = listener; calls.webRequest.push('sendHeaders'); } },
      onBeforeRedirect: { addListener(listener) { events.redirect = listener; calls.webRequest.push('redirect'); } },
      onCompleted: { addListener(listener) { events.completed = listener; calls.webRequest.push('completed'); } },
      onErrorOccurred: { addListener(listener) { events.errorOccurred = listener; calls.webRequest.push('errorOccurred'); } }
    },
    cookies: {
      getAll: async (details) => {
        calls.cookieQueries.push(details);
        return typeof cookieResults === 'function' ? cookieResults(details) : cookieResults;
      }
    },
    browserAction: {
      async setBadgeText(details) { calls.badgeText.push(details); },
      async setBadgeBackgroundColor(details) { calls.badgeColor.push(details); },
      async setTitle(details) { calls.badgeTitles.push(details); },
      async setIcon(details) { calls.badgeIcons.push(details); }
    },
    runtime: {
      connectNative: () => {
        calls.nativeConnects += 1;
        let messageListener = null;
        let disconnectListener = null;
        let disconnected = false;
        const disconnect = () => {
          if (disconnected) return;
          disconnected = true;
          calls.nativeDisconnects += 1;
          if (disconnectListener) disconnectListener();
        };
        return {
          onMessage: { addListener(listener) { messageListener = listener; } },
          onDisconnect: { addListener(listener) { disconnectListener = listener; } },
          postMessage(message) {
            calls.nativeMessages.push(message);
            const attempt = calls.nativeMessages.length;
            const deliver = () => {
              if (nativeDisconnect || attempt <= nativeFailuresBeforeSuccess) {
                disconnect();
                return;
              }
              let response;
              try {
                response = nativeResponse
                  ? nativeResponse(message)
                  : { type: 'defaults', request_id: message.request_id, target_dir: '' };
              } catch (_error) {
                disconnect();
                return;
              }
              if (response && response.request_id === undefined) {
                response.request_id = message.request_id;
              }
              if (messageListener && !disconnected) messageListener(response);
            };
            if (nativeDelayMs > 0) {
              setTimeout(deliver, nativeDelayMs);
            } else {
              queueMicrotask(deliver);
            }
          },
          disconnect,
        };
      },
      onMessage: { addListener(listener) { events.message = listener; } },
      sendMessage: async () => ({ ok: true })
    },
    storage: { local: {
      async get() { return {}; },
      async set() {}
    } },
    notifications: {
      async create(details) { calls.notifications.push(details); }
    }
  };
  return { browser, events, calls };
}

function makeDelayedNativeBrowser() {
  return makeFakeBrowser({
    nativeDelayMs: 1,
    nativeResponse: (message) => ({
      type: message.type === 'get_defaults' ? 'defaults' : 'task_list',
      request_id: message.request_id,
      target_dir: 'C:\\Downloads',
      tasks: []
    })
  });
}

test('reuses one native port for multiple background requests', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => ({
      type: 'defaults', request_id: message.request_id, target_dir: 'C:\\Downloads'
    })
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(result.ok, true);
  assert.equal(fake.calls.nativeMessages.length, 1);
  await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(fake.calls.nativeConnects, 1);
});

test('shares one native port across concurrent calls', async () => {
  const fake = makeDelayedNativeBrowser();
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await Promise.all([
    background.handleRuntimeMessage({ type: 'get-defaults' }),
    background.handleRuntimeMessage({ type: 'get-task-summary' })
  ]);
  assert.equal(fake.calls.nativeConnects, 1);
});
test('passive badge queries never carry a start intent', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: () => ({ type: 'task_list', tasks: [] })
  });
  const background = createBackground(fake.browser, {
    attempts: 1,
    delayMs: 0,
    timers: false,
    now: () => 900
  });
  await background.handleRuntimeMessage({ type: 'get-task-summary' });
  await background.refreshTaskStatus();
  assert.equal(fake.calls.nativeMessages.every((message) =>
    message.auto_start === true &&
    message.start_intent_unix_ms === undefined
  ), true);
});

test('explicit popup and settings actions carry a start intent', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => message.type === 'get-defaults'
      ? {
        type: 'defaults',
        request_id: message.request_id,
        target_dir: 'C:\\Downloads'
      }
      : { type: 'task_list', request_id: message.request_id, tasks: [] }
  });
  const background = createBackground(fake.browser, {
    attempts: 1,
    delayMs: 0,
    now: () => 1000,
    timers: false
  });
  await background.handleRuntimeMessage({ type: 'get-defaults', autoStart: true, startIntentUnixMs: 999 });
  await background.handleRuntimeMessage({
    type: 'get-defaults',
    autoStart: true,
    startIntentUnixMs: 1000
  });
  await background.handleRuntimeMessage({ type: 'get-task-summary', autoStart: true, startIntentUnixMs: 1001 });
  await background.handleRuntimeMessage({ type: 'get-task-summary', autoStart: true });
  assert.equal(fake.calls.nativeMessages[0].auto_start, true);
  assert.equal(fake.calls.nativeMessages[0].start_intent_unix_ms, 999);
  assert.equal(fake.calls.nativeMessages[1].auto_start, true);
  assert.equal(fake.calls.nativeMessages[1].start_intent_unix_ms, 1000);
  assert.equal(fake.calls.nativeMessages[2].start_intent_unix_ms, 1001);
  assert.equal(fake.calls.nativeMessages[3].start_intent_unix_ms, undefined);
});

test('new download is always an explicit GUI start intent', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 8 }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, {
    attempts: 1,
    delayMs: 0,
    timers: false,
    now: () => 700
  });
  await background.handleCreatedDownload({
    id: 3,
    url: 'https://example.test/a.zip',
    filename: 'a.zip'
  });
  await background.handleRuntimeMessage({
    type: 'submit-external',
    downloadId: 3,
    startIntentUnixMs: 700,
    form: {
      filename: 'a.zip',
      targetDir: 'C:\\Downloads',
      proxy: { enabled: false }
    }
  });
  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.equal(enqueue.auto_start, true);
  assert.equal(enqueue.start_intent_unix_ms, 700);
  assert.equal(fake.calls.nativeMessages.some((message) => message.type === 'show_task'), false);
  assert.equal(fake.calls.nativeMessages.some((message) => message.type === 'show_window'), false);
});
test('stops background badge polling when the task list has no active tasks', async () => {
  const fake = makeFakeBrowser({ nativeResponse: () => ({ type: 'task_list', tasks: [] }) });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  const result = await background.handleRuntimeMessage({ type: 'get-task-summary' });
  assert.equal(result.ok, true);
  assert.equal(background.isBadgeSyncRunning(), false);
  assert.deepEqual(fake.calls.badgeText.at(-1), { text: '' });
});

test('restarts the GUI minimized after a closed-GUI pipe failure while tasks are active', async () => {
  const fake = makeFakeBrowser({ nativeResponse: () => ({ type: 'task_list', tasks: [
    { task_id: 1, status: 'downloading', downloaded: 50, total_size: 100 }
  ] }) });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  await background.refreshTaskStatus();
  assert.equal(fake.calls.nativeMessages[0].auto_start, true);
  assert.equal(fake.calls.nativeMessages[0].start_intent_unix_ms, undefined);
  assert.equal(background.isBadgeSyncRunning(), true);
  assert.deepEqual(fake.calls.badgeText.at(-1), { text: '50%/1' });
});
test('popup refresh uses cached tasks during restart backoff instead of relaunching the GUI', async () => {
  let unavailable = false;
  const fake = makeFakeBrowser({
    nativeResponse: (message) => {
      if (unavailable) throw new Error('GUI pipe unavailable');
      return {
        type: 'task_list',
        request_id: message.request_id,
        tasks: [{ task_id: 1, status: 'downloading', downloaded: 50, total_size: 100 }]
      };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  await background.refreshTaskStatus();
  unavailable = true;

  await background.refreshTaskStatus();
  const callsAfterFailure = fake.calls.nativeMessages.length;
  const result = await background.handleRuntimeMessage({ type: 'get-task-summary' });

  assert.equal(fake.calls.nativeMessages.length, callsAfterFailure);
  assert.equal(result.ok, true);
  assert.equal(result.tasks[0].task_id, 1);
});
test('popup refresh backs off after an initial GUI startup failure without cached tasks', async () => {
  const fake = makeFakeBrowser({ nativeDisconnect: true });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });

  const first = await background.handleRuntimeMessage({ type: 'get-task-summary' });
  assert.equal(first.ok, false);
  const callsAfterFailure = fake.calls.nativeMessages.length;
  const second = await background.handleRuntimeMessage({ type: 'get-task-summary' });

  assert.equal(fake.calls.nativeMessages.length, callsAfterFailure);
  assert.equal(second.ok, false);
});

test('new popup start intent bypasses transient restart backoff', async () => {
  const fake = makeFakeBrowser({ nativeDisconnect: true });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });

  const first = await background.handleRuntimeMessage({ type: 'get-task-summary' });
  assert.equal(first.ok, false);
  const callsAfterFailure = fake.calls.nativeMessages.length;

  const explicit = await background.handleRuntimeMessage({
    type: 'get-task-summary',
    autoStart: true,
    startIntentUnixMs: 2000
  });
  assert.equal(explicit.ok, false);
  assert.ok(fake.calls.nativeMessages.length > callsAfterFailure);
  assert.equal(fake.calls.nativeMessages.at(-1).start_intent_unix_ms, 2000);
});

test('supported download pauses and opens one settings tab', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser);
  await background.handleCreatedDownload({
    id: 1,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  assert.deepEqual(fake.calls.pause, [1]);
  assert.equal(fake.calls.tabs.length, 1);
  assert.match(fake.calls.tabs[0].url, /settings\.html\?downloadId=1$/);
});

test('enqueue refreshes cookies for the exact URL, store, and first-party partition', async () => {
  const fake = makeFakeBrowser({
    sourceTabs: [{ id: 4, url: 'https://app.example.test/page', incognito: false, cookieStoreId: 'firefox-default' }],
    cookieResults: [{
      name: 'session', value: 'fresh-secret', storeId: 'firefox-default', firstPartyDomain: ''
    }],
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 12 }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false, now: () => 1000 });
  fake.events.sendHeaders({
    requestId: 'firefox-request-1',
    method: 'GET',
    url: 'https://files.example.test/a.pdf',
    tabId: 4,
    documentUrl: 'https://app.example.test/page',
    requestHeaders: [
      { name: 'Referer', value: 'https://app.example.test/page' }
    ]
  });
  await background.handleCreatedDownload({
    id: 22,
    url: 'https://files.example.test/a.pdf',
    referrer: 'https://app.example.test/page',
    tabId: 4,
    incognito: false,
    cookieStoreId: 'firefox-default',
    filename: 'a.pdf'
  });
  const submitted = await background.submitExternalDownload(22, {
    filename: 'a.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });
  assert.equal(submitted.ok, true);
  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.deepEqual(enqueue.request_context, {
    headers: [
      { name: 'Referer', value: 'https://app.example.test/page' },
      { name: 'Cookie', value: 'session=fresh-secret' }
    ],
    source_page_url: 'https://app.example.test/page',
    initial_url: 'https://files.example.test/a.pdf',
    final_url: 'https://files.example.test/a.pdf',
    incognito: false,
    cookie_store_id: 'firefox-default'
  });
  assert.deepEqual(fake.calls.cookieQueries, [{
    url: 'https://files.example.test/a.pdf',
    storeId: 'firefox-default',
    firstPartyDomain: ''
  }]);
  assert.deepEqual(fake.calls.webRequest, ['sendHeaders', 'redirect', 'completed', 'errorOccurred']);
});

test('enqueue drops captured authorization when final request URL differs from download URL', async () => {
  const fake = makeFakeBrowser({
    cookieResults: (details) => details.url === 'https://cdn.test/file'
      ? [{ name: 'cdn-cookie', value: 'for-final-host', storeId: 'firefox-default', firstPartyDomain: '' }]
      : [{ name: 'origin-cookie', value: 'must-not-replay', storeId: 'firefox-default', firstPartyDomain: '' }],
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 13 }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'redirected', method: 'GET', url: 'https://origin.test/file', tabId: 4,
    documentUrl: 'https://app.test/page',
    requestHeaders: [
      { name: 'Authorization', value: 'Bearer origin-secret' },
      { name: 'X-Auth-Token', value: 'origin-token-secret' }
    ]
  });
  fake.events.redirect({
    requestId: 'redirected', url: 'https://origin.test/file', redirectUrl: 'https://cdn.test/file'
  });
  await background.handleCreatedDownload({
    id: 23, url: 'https://origin.test/file', filename: 'file', cookieStoreId: 'firefox-default'
  });
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 23 });
  assert.equal(pending.download.url, 'https://origin.test/file');
  await background.submitExternalDownload(23, {
    filename: 'file', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.equal(enqueue.url, 'https://cdn.test/file');
  assert.equal(enqueue.request_context.initial_url, 'https://origin.test/file');
  assert.equal(enqueue.request_context.final_url, 'https://cdn.test/file');
  assert.deepEqual(fake.calls.cookieQueries.map((query) => query.url), ['https://cdn.test/file']);
  assert.deepEqual(enqueue.request_context.headers, [
    { name: 'Cookie', value: 'cdn-cookie=for-final-host' }
  ]);
});

test('private container cookies are queried only from that download store', async () => {
  const fake = makeFakeBrowser({
    sourceTabs: [{ id: 9, url: 'https://private.app.test/page', incognito: true, cookieStoreId: 'firefox-private' }],
    cookieResults: [{
      name: 'private-session', value: 'private-value',
      storeId: 'firefox-private', firstPartyDomain: ''
    }],
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 14 }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'private-download', method: 'GET', url: 'https://files.test/private.pdf', tabId: 9,
    documentUrl: 'https://private.app.test/page', requestHeaders: []
  });
  await background.handleCreatedDownload({
    id: 24, url: 'https://files.test/private.pdf', filename: 'private.pdf',
    tabId: 9, incognito: true, cookieStoreId: 'firefox-private'
  });
  await background.submitExternalDownload(24, {
    filename: 'private.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.equal(enqueue.request_context.incognito, true);
  assert.equal(enqueue.request_context.cookie_store_id, 'firefox-private');
  assert.deepEqual(fake.calls.cookieQueries, [{
    url: 'https://files.test/private.pdf',
    storeId: 'firefox-private',
    firstPartyDomain: ''
  }]);
  assert.deepEqual(enqueue.request_context.headers.find((header) => header.name === 'Cookie'), {
    name: 'Cookie', value: 'private-session=private-value'
  });
});

test('cookie enrichment discards results from another store or partition', async () => {
  const fake = makeFakeBrowser({
    cookieResults: [
      { name: 'valid', value: 'default-store', storeId: 'firefox-default', firstPartyDomain: '' },
      { name: 'wrong-store', value: 'must-not-leak', storeId: 'firefox-container-2', firstPartyDomain: '' },
      {
        name: 'wrong-partition', value: 'must-not-leak', storeId: 'firefox-default',
        firstPartyDomain: '', partitionKey: { topLevelSite: 'https://other.test' }
      }
    ],
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 15 }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'partition-filter', method: 'GET', url: 'https://files.test/partition.pdf',
    tabId: 4, documentUrl: 'https://app.test/page', requestHeaders: []
  });
  await background.handleCreatedDownload({
    id: 25, url: 'https://files.test/partition.pdf', filename: 'partition.pdf',
    tabId: 4, cookieStoreId: 'firefox-default'
  });
  await background.submitExternalDownload(25, {
    filename: 'partition.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.deepEqual(enqueue.request_context.headers.filter((header) => header.name === 'Cookie'), [
    { name: 'Cookie', value: 'valid=default-store' }
  ]);
});

test('an empty cookie query preserves a captured request cookie', async () => {
  const fake = makeFakeBrowser({ cookieResults: [] });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'empty-cookie-query', method: 'GET', url: 'https://files.test/kept.pdf',
    tabId: 4, documentUrl: 'https://app.test/page',
    requestHeaders: [{ name: 'Cookie', value: 'session=captured-valid' }]
  });
  await background.handleCreatedDownload({
    id: 26, url: 'https://files.test/kept.pdf', filename: 'kept.pdf', cookieStoreId: 'firefox-default'
  });
  await background.submitExternalDownload(26, {
    filename: 'kept.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.deepEqual(enqueue.request_context.headers.find((header) => header.name === 'Cookie'), {
    name: 'Cookie', value: 'session=captured-valid'
  });
});

test('a cookie API failure preserves the captured request cookie', async () => {
  const fake = makeFakeBrowser({ cookieResults: () => { throw new Error('cookies permission unavailable'); } });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'cookie-query-error', method: 'GET', url: 'https://files.test/fallback.pdf',
    tabId: 4, documentUrl: 'https://app.test/page',
    requestHeaders: [{ name: 'Cookie', value: 'session=fallback-valid' }]
  });
  await background.handleCreatedDownload({
    id: 27, url: 'https://files.test/fallback.pdf', filename: 'fallback.pdf', cookieStoreId: 'firefox-default'
  });
  await background.submitExternalDownload(27, {
    filename: 'fallback.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const enqueue = fake.calls.nativeMessages.find((message) => message.type === 'enqueue');
  assert.deepEqual(enqueue.request_context.headers.find((header) => header.name === 'Cookie'), {
    name: 'Cookie', value: 'session=fallback-valid'
  });
});

test('cookie enrichment does not mutate the captured request context', async () => {
  const context = {
    headers: [{ name: 'Cookie', value: 'captured=value' }],
    sourcePageUrl: 'https://files.test/page',
    initialUrl: 'https://files.test/immutable.pdf',
    finalUrl: 'https://files.test/immutable.pdf',
    tabId: null,
    incognito: false,
    cookieStoreId: 'firefox-default'
  };
  const originalContext = structuredClone(context);
  const fake = makeFakeBrowser({ cookieResults: [] });
  const requestTracker = {
    claimDownload: () => context,
    observeSendHeaders() {},
    observeRedirect() {},
    observeComplete() {},
    observeError() {}
  };
  const background = createBackground(fake.browser, {
    attempts: 1, delayMs: 0, timers: false, requestTracker
  });
  await background.handleCreatedDownload({
    id: 28, url: context.finalUrl, filename: 'immutable.pdf', cookieStoreId: 'firefox-default'
  });
  await background.submitExternalDownload(28, {
    filename: 'immutable.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });
  assert.deepEqual(context, originalContext);
});

test('reauthorization focuses the original tab and forwards one fresh request', async () => {
  const fake = makeFakeBrowser({
    sourceTabs: [{ id: 4, incognito: false, cookieStoreId: 'firefox-default' }],
    cookieResults: (details) => details.url.includes('sig=new')
      ? [{ name: 'session', value: 'fresh-from-cookie-api', storeId: 'firefox-default', firstPartyDomain: '' }]
      : [],
    nativeResponse: (message) => {
      if (message.type === 'enqueue') return { type: 'enqueue_result', ok: true, task_id: 42 };
      if (message.type === 'refresh_firefox_authorization') return { type: 'action_result', ok: true };
      if (message.type === 'get_defaults') return { type: 'defaults', target_dir: 'C:\\Downloads' };
      return { type: 'task_list', tasks: [] };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0, timers: false });
  fake.events.sendHeaders({
    requestId: 'initial', method: 'GET', url: 'https://files.test/file.pdf?sig=old', tabId: 4,
    documentUrl: 'https://app.test/page?view=1',
    requestHeaders: [{ name: 'Cookie', value: 'session=old' }]
  });
  await background.handleCreatedDownload({
    id: 5,
    url: 'https://files.test/file.pdf?sig=old',
    referrer: 'https://app.test/page?view=1',
    tabId: 4,
    incognito: false,
    cookieStoreId: 'firefox-default',
    filename: 'file.pdf'
  });
  await background.submitExternalDownload(5, {
    filename: 'file.pdf', targetDir: 'C:\\Downloads', proxy: { enabled: false }
  });

  const started = await background.handleRuntimeMessage({
    type: 'reauthorize-firefox', taskId: 42
  });
  assert.equal(started.ok, true);
  assert.equal(started.waiting, true);
  assert.deepEqual(fake.calls.tabUpdates, [{ id: 4, details: { active: true } }]);

  fake.events.sendHeaders({
    requestId: 'fresh', method: 'GET', url: 'https://files.test/file.pdf?sig=new', tabId: 4,
    documentUrl: 'https://app.test/page?view=1',
    requestHeaders: []
  });
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (fake.calls.nativeMessages.some((message) => message.type === 'refresh_firefox_authorization')) break;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  const refresh = fake.calls.nativeMessages.find((message) => message.type === 'refresh_firefox_authorization');
  assert.ok(refresh);
  assert.equal(refresh.task_id, 42);
  assert.deepEqual(refresh.request_context.headers, [
    { name: 'Cookie', value: 'session=fresh-from-cookie-api' }
  ]);
  assert.ok(fake.calls.cookieQueries.some((query) => (
    query.url === 'https://files.test/file.pdf?sig=new'
      && query.storeId === 'firefox-default'
      && query.firstPartyDomain === ''
  )));
});

test('closing settings after an enqueue timeout does not restore Firefox blindly', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: false, error: { code: 'engine_timeout' } }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const created = await background.handleCreatedDownload({
    id: 4,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const result = await background.submitExternalDownload(4, {
    filename: 'file.zip',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false }
  });
  assert.equal(result.code, 'enqueue_pending');
  fake.events.removed(created.tabId);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(fake.calls.resume, []);
  assert.deepEqual(fake.calls.download, []);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 4 });
  assert.equal(pending.ok, true);
  assert.ok(fake.calls.notifications.some((item) => item.message.includes('尚未確認')));
});

test('a failed retry after enqueue timeout keeps Firefox handoff pending', async () => {
  let enqueueAttempts = 0;
  const fake = makeFakeBrowser({
    nativeResponse: (message) => {
      if (message.type === 'enqueue') {
        enqueueAttempts += 1;
        return enqueueAttempts === 1
          ? { type: 'enqueue_result', ok: false, error: { code: 'engine_timeout' } }
          : { type: 'error', error: { code: 'native_unavailable', message: '暫時無法連線' } };
      }
      return { type: 'task_list', tasks: [] };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await background.handleCreatedDownload({
    id: 6,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const form = {
    filename: 'file.zip',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false }
  };
  const first = await background.submitExternalDownload(6, form);
  assert.equal(first.code, 'enqueue_pending');
  const second = await background.submitExternalDownload(6, form);
  assert.equal(second.code, 'enqueue_pending');
  assert.deepEqual(fake.calls.download, []);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 6 });
  assert.equal(pending.ok, true);
});

test('closing settings while enqueue is in flight does not race Firefox fallback', async () => {
  const fake = makeFakeBrowser({
    nativeDelayMs: 5,
    nativeResponse: (message) => message.type === 'enqueue'
      ? { type: 'enqueue_result', ok: true, task_id: 44, awaiting_file_decision: false }
      : { type: 'task_list', tasks: [] }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const created = await background.handleCreatedDownload({
    id: 5,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const submit = background.submitExternalDownload(5, {
    filename: 'file.zip',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false }
  });
  fake.events.removed(created.tabId);
  const result = await submit;
  assert.equal(result.ok, true);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(fake.calls.download, []);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 5 });
  assert.equal(pending.ok, false);
});

test('Native host failure resumes the original Firefox download', async () => {
  const fake = makeFakeBrowser({ nativeDisconnect: true });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await background.handleCreatedDownload({
    id: 2,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const result = await background.submitExternalDownload(2, {
    filename: 'file.zip',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false, protocol: 'http', host: '', port: '8080', username: '', password: 'secret' }
  });
  assert.equal(result.ok, false);
  assert.deepEqual(fake.calls.resume, [2]);
  assert.equal(fake.calls.notifications.length, 1);
  assert.equal(fake.calls.notifications[0].message.includes('secret'), false);
});

test('a rejected blob enqueue preserves its error code and the next page click opens fresh settings', async () => {
  let enqueueCount = 0;
  const fake = makeFakeBrowser({
    nativeResponse: (message) => {
      if (message.type === 'enqueue') {
        enqueueCount += 1;
        return enqueueCount === 1
          ? { type: 'error', error: { code: 'invalid_request_context', message: 'redacted test cause' } }
          : { type: 'enqueue_result', ok: true, task_id: 92, awaiting_file_decision: false };
      }
      return { type: 'task_list', tasks: [] };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const url = 'https://files.example.test/blob-bridge.pdf';
  const observeRequest = (requestId) => fake.events.sendHeaders({ requestId, method: 'GET', tabId: 7,
    frameId: 0, url, documentUrl: pageSender.url,
    requestHeaders: [{ name: 'Authorization', value: 'Bearer fictional' }] });
  observeRequest('blob-reject-one');
  const created = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url, filename: 'report.pdf', fallbackToken: 'retry-blob-one' }, pageSender);
  const form = {
    filename: 'report.pdf', targetDir: 'C:\\Downloads', segments: 4,
    proxy: { enabled: false }
  };

  const rejected = await background.submitExternalDownload(created.downloadId, form);
  assert.equal(rejected.ok, false);
  assert.equal(rejected.code, 'invalid_request_context');
  assert.equal(rejected.nativeErrorCode, 'invalid_request_context');
  assert.equal(rejected.firefoxRestored, true);
  assert.match(rejected.error, /invalid_request_context/);
  assert.equal(rejected.error.startsWith('Native host'), true);
  assert.match(fake.calls.notifications[0].message, /invalid_request_context/);
  assert.equal((await background.handleRuntimeMessage({ type: 'get-pending', downloadId: created.downloadId })).ok, false);
  assert.equal(fake.calls.pageMessages.at(-1).message.type, 'restore-page-download');

  observeRequest('blob-reject-two');
  const retry = await background.handleRuntimeMessage({ type: 'intercept-page-download',
    url, filename: 'report.pdf', fallbackToken: 'retry-blob-two' }, pageSender);
  assert.equal(retry.ok, true);
  assert.equal(fake.calls.tabs.length, 2);
  const retried = await background.submitExternalDownload(retry.downloadId, form);
  assert.equal(retried.ok, true);
  assert.equal((await background.handleRuntimeMessage({ type: 'get-pending', downloadId: retry.downloadId })).ok, false);
  assert.equal(fake.calls.pageMessages.at(-1).message.type, 'release-page-download');
  assert.deepEqual(fake.calls.download, []);
});

test('managed fallback onCreated event does not re-enter interception', async () => {
  const fake = makeFakeBrowser({ resumeFails: true });
  const background = createBackground(fake.browser);
  await background.handleCreatedDownload({
    id: 3,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  await background.restoreFirefoxDownload({
    downloadId: 3,
    url: 'https://example.test/file.zip',
    filename: 'file.zip',
    forceRecreate: true
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fake.calls.download.length, 1);
  assert.equal(fake.calls.download[0].saveAs, false);
  assert.deepEqual(fake.calls.pause, [3]);
});

test('Native host retry succeeds after Curl Downloader starts', async () => {
  const fake = makeFakeBrowser({
    nativeFailuresBeforeSuccess: 2,
    nativeResponse: (message) => ({
      type: 'defaults',
      request_id: message.request_id,
      target_dir: 'C:\\Downloads'
    })
  });
  const background = createBackground(fake.browser, { attempts: 5, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(result.ok, true);
  assert.equal(result.targetDir, 'C:\\Downloads');
  assert.equal(fake.calls.nativeMessages.length, 3);
});

test('Native host retry stops after five attempts', async () => {
  const fake = makeFakeBrowser({ nativeFailuresBeforeSuccess: Infinity });
  const background = createBackground(fake.browser, { attempts: 5, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(result.ok, false);
  assert.equal(result.code, 'native_unavailable');
  assert.equal(fake.calls.nativeMessages.length, 5);
});

test('pick-folder maps native directory to settings camelCase', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => ({
      type: 'folder',
      request_id: message.request_id,
      ok: true,
      target_dir: 'D:\\Downloads',
      error: null
    })
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'pick-folder', downloadId: 1 });
  assert.equal(result.ok, true);
  assert.equal(result.targetDir, 'D:\\Downloads');
});

test('cancel-download cancels and erases paused Firefox item', async () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await background.handleCreatedDownload({
    id: 7,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const result = await background.handleRuntimeMessage({ type: 'cancel-download', downloadId: 7 });
  assert.equal(result.ok, true);
  assert.deepEqual(fake.calls.cancel, [7]);
  assert.deepEqual(fake.calls.erase, [{ id: 7 }]);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 7 });
  assert.equal(pending.ok, false);
});

test('task controls bridge list, show, file, and folder actions', async () => {
  const messages = [];
  const fake = makeFakeBrowser({
    nativeResponse: (message) => {
      messages.push(message);
      if (message.type === 'list_tasks') {
        return {
          type: 'task_list',
          request_id: message.request_id,
          tasks: [{
            task_id: 7,
            filename: 'file.zip',
            status: 'downloading',
            downloaded: 512,
            total_size: 1024,
            current_bps: 128,
            average_bps: 64,
            eta_seconds: 4,
            target_dir: 'C:\Downloads',
            file_available: false,
            folder_available: true
          }]
        };
      }
      return { type: 'action_result', request_id: message.request_id, ok: true, error: null };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });

  const list = await background.handleRuntimeMessage({ type: 'get-task-summary' });
  assert.equal(list.ok, true);
  assert.equal(list.tasks[0].task_id, 7);
  assert.equal(list.tasks[0].downloaded, 512);
  assert.equal(list.tasks[0].target_dir, 'C:\Downloads');
  assert.equal(list.tasks[0].folder_available, true);
  for (const type of ['show-task', 'open-file', 'open-folder']) {
    const result = await background.handleRuntimeMessage({ type, taskId: 7 });
    assert.equal(result.ok, true);
  }
  assert.deepEqual(messages.map((message) => message.type), [
    'list_tasks',
    'show_task',
    'open_file',
    'open_folder'
  ]);
  assert.equal(messages.every((message) => message.task_id === undefined || message.task_id === 7), true);
  assert.equal(messages.slice(1).every((message) => message.start_intent_unix_ms === undefined), true);
});

test('task control errors stay in popup flow without Firefox fallback', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => ({
      type: 'action_result',
      request_id: message.request_id,
      ok: false,
      error: { code: 'file_unavailable', message: '檔案尚未完成。' }
    })
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'open-file', taskId: 7 });
  assert.deepEqual(result, { ok: false, error: '檔案尚未完成。' });
  assert.deepEqual(fake.calls.resume, []);
  assert.deepEqual(fake.calls.download, []);
});
test('popup lifecycle keeps one native session alive only while open', async () => {
  const fake = makeFakeBrowser();
  const keepAliveCalls = [];
  const session = {
    setKeepAlive(value) { keepAliveCalls.push(Boolean(value)); },
    send: async () => ({ type: 'task_list', request_id: 'session', tasks: [] }),
    close() {}
  };
  const background = createBackground(fake.browser, {
    nativeSession: session,
    timers: false
  });

  const opened = await background.handleRuntimeMessage({ type: 'popup-open' });
  const closed = await background.handleRuntimeMessage({ type: 'popup-close' });

  assert.deepEqual(opened, { ok: true });
  assert.deepEqual(closed, { ok: true });
  assert.deepEqual(keepAliveCalls, [true, false]);
});

test('manually stopped native host stops polling and closes its session', async () => {
  const fake = makeFakeBrowser();
  let closeCalls = 0;
  const session = {
    setKeepAlive() {},
    send: async () => ({
      type: 'error',
      request_id: 'manual-stop',
      error: { code: 'manually_stopped', message: 'Curl Downloader 已由使用者關閉' }
    }),
    close() { closeCalls += 1; }
  };
  const background = createBackground(fake.browser, { nativeSession: session, timers: false });

  const result = await background.handleRuntimeMessage({ type: 'get-task-summary' });
  assert.deepEqual(result, {
    ok: false,
    code: 'manually_stopped',
    error: 'Curl Downloader 已由使用者關閉'
  });
  assert.equal(background.isBadgeSyncRunning(), false);
  assert.equal(closeCalls, 1);
});

test('download segment count must be an integer from one through eight', () => {
  const fake = makeFakeBrowser();
  const background = createBackground(fake.browser, { timers: false });
  const base = {
    filename: 'file.bin',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false }
  };
  assert.equal(background.validateForm({ ...base, segments: 1 }), null);
  assert.equal(background.validateForm({ ...base, segments: 8 }), null);
  assert.match(background.validateForm({ ...base, segments: 0 }), /1 至 8/);
  assert.match(background.validateForm({ ...base, segments: 9 }), /1 至 8/);
  assert.match(background.validateForm({ ...base, segments: 2.5 }), /整數/);
});

test('pause failure resumes the original Firefox item without duplicating it', async () => {
  const fake = makeFakeBrowser({ pauseFails: true, eraseFails: true });
  const background = createBackground(fake.browser);
  const result = await background.handleCreatedDownload({
    id: 11,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  assert.deepEqual(fake.calls.pause, [11]);
  assert.deepEqual(fake.calls.cancel, []);
  assert.deepEqual(fake.calls.erase, []);
  assert.deepEqual(fake.calls.resume, [11]);
  assert.deepEqual(fake.calls.download, []);
  assert.deepEqual(result, { restored: true });
});

test('erase failure after pause keeps the settings page available for retry', async () => {
  const fake = makeFakeBrowser({ eraseFails: true });
  const background = createBackground(fake.browser);
  const result = await background.handleCreatedDownload({
    id: 12,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  assert.deepEqual(fake.calls.pause, [12]);
  assert.deepEqual(fake.calls.cancel, [12]);
  assert.equal(fake.calls.erase.length, 3);
  assert.deepEqual(fake.calls.resume, []);
  assert.deepEqual(fake.calls.download, []);
  assert.deepEqual(result, { paused: true, tabId: 10 });
});

test('Firefox restore failure keeps the pending item for a visible retry', async () => {
  const fake = makeFakeBrowser({ downloadFails: true });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await background.handleCreatedDownload({
    id: 13,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const result = await background.handleRuntimeMessage({
    type: 'restore-firefox',
    downloadId: 13
  });
  assert.equal(result.ok, false);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 13 });
  assert.equal(pending.ok, true);
});

test('accepted Curl task is cancelled before Firefox fallback when native erase fails', async () => {
  const fake = makeFakeBrowser({
    eraseFails: true,
    nativeResponse: (message) => {
      if (message.type === 'enqueue') {
        return { type: 'enqueue_result', ok: true, task_id: 91, awaiting_file_decision: false };
      }
      if (message.type === 'cancel_task') {
        return { type: 'action_result', ok: true };
      }
      return { type: 'task_list', tasks: [] };
    }
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await background.handleCreatedDownload({
    id: 15,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  const result = await background.submitExternalDownload(15, {
    filename: 'file.zip',
    targetDir: 'C:\\Downloads',
    proxy: { enabled: false }
  });
  assert.equal(result.ok, false);
  assert.equal(result.code, 'firefox_cleanup_failed');
  assert.equal(result.taskCancelled, true);
  assert.deepEqual(fake.calls.nativeMessages.map((message) => message.type), [
    'enqueue',
    'cancel_task'
  ]);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 15 });
  assert.equal(pending.ok, true);
});

test('settings-tab failure reports an unrecoverable Firefox handoff without losing pending state', async () => {
  const fake = makeFakeBrowser({ tabCreateFails: true, resumeFails: true, downloadFails: true });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const result = await background.handleCreatedDownload({
    id: 14,
    url: 'https://example.test/file.zip',
    filename: 'file.zip'
  });
  assert.deepEqual(result, {
    restored: false,
    error: 'Firefox 下載未能恢復；請重新開啟下載設定頁後重試。'
  });
  assert.ok(fake.calls.notifications.length >= 1);
  const pending = await background.handleRuntimeMessage({ type: 'get-pending', downloadId: 14 });
  assert.equal(pending.ok, true);
});
