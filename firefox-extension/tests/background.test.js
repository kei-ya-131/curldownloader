const test = require('node:test');
const assert = require('node:assert/strict');
const createBackground = require('../background.js');

function makeFakeBrowser({
  resumeFails = false,
  pauseFails = false,
  eraseFails = false,
  downloadFails = false,
  tabCreateFails = false,
  nativeDisconnect = false,
  nativeFailuresBeforeSuccess = 0,
  nativeDelayMs = 0,
  nativeResponse = null
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
    pause: [],
    resume: [],
    cancel: [],
    erase: [],
    download: [],
    tabs: [],
    notifications: [],
    webRequest: [],
    nativeMessages: [],
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
      onRemoved: { addListener(listener) { events.removed = listener; } },
      create: async (details) => {
        if (tabCreateFails) throw new Error('tab create failed');
        const tab = { id: nextTabId++, ...details };
        calls.tabs.push(tab);
        return tab;
      }
    },
    webRequest: {
      onSendHeaders: { addListener(listener) { events.sendHeaders = listener; calls.webRequest.push('sendHeaders'); } },
      onBeforeRedirect: { addListener(listener) { events.redirect = listener; calls.webRequest.push('redirect'); } },
      onCompleted: { addListener(listener) { events.completed = listener; calls.webRequest.push('completed'); } },
      onErrorOccurred: { addListener(listener) { events.errorOccurred = listener; calls.webRequest.push('errorOccurred'); } }
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
              const timer = setTimeout(deliver, nativeDelayMs);
              if (typeof timer.unref === 'function') timer.unref();
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

test('sniffed Firefox request context is attached to the matching enqueue', async () => {
  const fake = makeFakeBrowser({
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
      { name: 'Cookie', value: 'session=secret' },
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
      { name: 'Cookie', value: 'session=secret' },
      { name: 'Referer', value: 'https://app.example.test/page' }
    ],
    source_page_url: 'https://app.example.test/page',
    initial_url: 'https://files.example.test/a.pdf',
    final_url: 'https://files.example.test/a.pdf',
    incognito: false,
    cookie_store_id: 'firefox-default'
  });
  assert.deepEqual(fake.calls.webRequest, ['sendHeaders', 'redirect', 'completed', 'errorOccurred']);
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
