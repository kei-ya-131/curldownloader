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
    nativeMessages: []
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
      sendNativeMessage: async (_hostName, message) => {
        calls.nativeMessages.push(message);
        if (nativeDisconnect) throw new Error('Native host disconnected');
        if (calls.nativeMessages.length <= nativeFailuresBeforeSuccess) {
          throw new Error('No such native application');
        }
        return nativeResponse
          ? nativeResponse(message)
          : { type: 'defaults', request_id: message.request_id, target_dir: '' };
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
  const fake = makeFakeBrowser({ nativeResponse: (message) => ({
    type: message.type === 'get_defaults' ? 'defaults' : 'task_list',
    request_id: message.request_id,
    target_dir: 'C:\\Downloads',
    tasks: []
  }) });
  let inFlight = 0;
  fake.maxNativeInFlight = 0;
  const original = fake.browser.runtime.sendNativeMessage;
  fake.browser.runtime.sendNativeMessage = async (...args) => {
    inFlight += 1;
    fake.maxNativeInFlight = Math.max(fake.maxNativeInFlight, inFlight);
    await new Promise((resolve) => setImmediate(resolve));
    const result = await original(...args);
    inFlight -= 1;
    return result;
  };
  return fake;
}

test('uses one-shot Native Messaging and never creates a persistent port', async () => {
  const fake = makeFakeBrowser({
    nativeResponse: (message) => ({
      type: 'defaults', request_id: message.request_id, target_dir: 'C:\\Downloads'
    })
  });
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  const result = await background.handleRuntimeMessage({ type: 'get-defaults' });
  assert.equal(result.ok, true);
  assert.equal(fake.calls.nativeMessages.length, 1);
});

test('serializes two Native calls so only one is in flight', async () => {
  const fake = makeDelayedNativeBrowser();
  const background = createBackground(fake.browser, { attempts: 1, delayMs: 0 });
  await Promise.all([
    background.handleRuntimeMessage({ type: 'get-defaults' }),
    background.handleRuntimeMessage({ type: 'get-task-summary' })
  ]);
  assert.equal(fake.maxNativeInFlight, 1);
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