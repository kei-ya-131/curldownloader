const test = require('node:test');
const assert = require('node:assert/strict');
const createBackground = require('../background.js');

function makeFakeBrowser({
  resumeFails = false,
  nativeDisconnect = false,
  nativeFailuresBeforeSuccess = 0,
  nativeResponse = null
} = {}) {
  const events = {
    created: null,
    removed: null,
    message: null
  };
  const calls = {
    pause: [],
    resume: [],
    cancel: [],
    erase: [],
    download: [],
    tabs: [],
    notifications: [],
    nativeConnects: 0
  };
  const port = {
    messages: [],
    onMessage: { addListener(listener) { port.messageListener = listener; } },
    onDisconnect: { addListener(listener) { port.disconnectListener = listener; } },
    postMessage(message) {
      port.messages.push(message);
      if (nativeDisconnect && port.disconnectListener) {
        queueMicrotask(() => port.disconnectListener());
      } else if (nativeResponse && port.messageListener) {
        queueMicrotask(() => port.messageListener(nativeResponse(message)));
      }
    }
  };
  let nextTabId = 10;
  let nextDownloadId = 100;
  const browser = {
    downloads: {
      onCreated: { addListener(listener) { events.created = listener; } },
      pause: async (id) => { calls.pause.push(id); },
      resume: async (id) => {
        calls.resume.push(id);
        if (resumeFails) throw new Error('resume failed');
      },
      cancel: async (id) => { calls.cancel.push(id); },
      erase: async (query) => { calls.erase.push(query); },
      download: async (details) => {
        calls.download.push(details);
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
      onRemoved: { addListener(listener) { events.removed = listener; } },
      create: async (details) => {
        const tab = { id: nextTabId++, ...details };
        calls.tabs.push(tab);
        return tab;
      }
    },
    runtime: {
      connectNative: () => {
        calls.nativeConnects += 1;
        if (calls.nativeConnects <= nativeFailuresBeforeSuccess) {
          throw new Error('No such native application');
        }
        return port;
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
  return { browser, events, calls, port };
}

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
  assert.equal(fake.calls.nativeConnects, 3);
});

test('Native host retry stops after five attempts', async () => {
  const fake = makeFakeBrowser({ nativeFailuresBeforeSuccess: Infinity });
  const background = createBackground(fake.browser, { attempts: 5, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(result.ok, false);
  assert.equal(result.code, 'native_unavailable');
  assert.equal(fake.calls.nativeConnects, 5);
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
