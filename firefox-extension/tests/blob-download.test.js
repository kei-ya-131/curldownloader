const test = require('node:test');
const assert = require('node:assert/strict');
const installBlobDownloadInterceptor = require('../blob-download.js');

function makeHarness(options = {}) {
  const messages = [];
  const revocations = [];
  const createCalls = [];
  const timerCallbacks = [];
  const clickListeners = [];
  const runtimeListeners = [];
  let nextObjectUrl = 1;
  let nextToken = 1;
  let nativeDownloads = 0;

  class FakeBlob {}
  class FakeResponse {
    constructor(url, ok, blob) {
      this.url = url;
      this.ok = ok;
      this.value = blob;
      this.blobCalls = 0;
    }

    blob() {
      this.blobCalls += 1;
      return Promise.resolve(this.value);
    }
  }

  class FakeAnchor {
    constructor() {
      this.tagName = 'A';
      this.href = '';
      this._download = '';
      this.downloadPresent = false;
      this.parentNode = null;
      this.style = {};
    }

    click() {
      const event = {
        target: this,
        defaultPrevented: false,
        preventDefault() { this.defaultPrevented = true; }
      };
      if (this.parentNode) {
        for (const listener of clickListeners) listener(event);
      }
      if (!event.defaultPrevented) nativeDownloads += 1;
    }

    get download() { return this._download; }
    set download(value) {
      this.downloadPresent = true;
      this._download = String(value);
    }
    hasAttribute(name) { return name === 'download' && this.downloadPresent; }
  }

  if (options.inheritedAnchorClick) {
    const inheritedClick = FakeAnchor.prototype.click;
    delete FakeAnchor.prototype.click;
    Object.setPrototypeOf(FakeAnchor.prototype, { click: inheritedClick });
  }

  const document = {
    addEventListener(type, listener, capture) {
      if (type === 'click' && capture) clickListeners.push(listener);
    },
    removeEventListener(type, listener) {
      if (type !== 'click') return;
      const index = clickListeners.indexOf(listener);
      if (index >= 0) clickListeners.splice(index, 1);
    },
    createElement(name) {
      assert.equal(name, 'a');
      return new FakeAnchor();
    },
    body: {
      appendChild(anchor) { anchor.parentNode = this; },
      removeChild(anchor) { anchor.parentNode = null; }
    },
    documentElement: null
  };
  const pageWindow = {
    document,
    Response: FakeResponse,
    HTMLAnchorElement: FakeAnchor,
    URL: {
      createObjectURL(blob) {
        createCalls.push(blob);
        return `blob:https://chatgpt.com/fake-${nextObjectUrl++}`;
      },
      revokeObjectURL(url) { revocations.push(url); }
    }
  };
  pageWindow.wrappedJSObject = pageWindow;
  const browserApi = {
    runtime: {
      sendMessage(message) {
        messages.push(message);
        const response = typeof options.sendMessageResponse === 'function'
          ? options.sendMessageResponse(message, messages.length)
          : (options.sendMessageResponse || { ok: true });
        return Promise.resolve(response);
      },
      onMessage: {
        addListener(listener) { runtimeListeners.push(listener); }
      }
    }
  };
  const interceptor = installBlobDownloadInterceptor(pageWindow, browserApi, {
    exportFunction: (fn) => fn,
    randomToken: () => `fallback-${nextToken++}`,
    setTimeout: (callback, delayMs) => { timerCallbacks.push({ callback, delayMs }); }
  });

  function sendRuntimeMessage(message) {
    return Promise.all(runtimeListeners.map((listener) => listener(message))).then((results) => results[0]);
  }

  function clickDownload(url, filename = 'download.bin') {
    const anchor = new FakeAnchor();
    anchor.href = url;
    anchor.download = filename;
    anchor.click();
    return anchor;
  }

  function userClickDownload(url, filename = 'download.bin') {
    const anchor = new FakeAnchor();
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    return anchor;
  }

  return {
    browserApi,
    pageWindow,
    messages,
    revocations,
    createCalls,
    timerCallbacks,
    get nativeDownloads() { return nativeDownloads; },
    makeBlob: () => new FakeBlob(),
    sendRuntimeMessage,
    clickDownload,
    userClickDownload,
    interceptor
  };
}

test('maps Response.blob to its exact HTTP response URL for the later object URL download', async () => {
  const harness = makeHarness();
  const blob = harness.makeBlob();
  const responseUrl = 'https://chatgpt.com/backend-api/files/final-file?token=fake';
  const response = new harness.pageWindow.Response(responseUrl, true, blob);

  const returnedBlob = await response.blob();
  const objectUrl = harness.pageWindow.URL.createObjectURL(returnedBlob);
  harness.userClickDownload(objectUrl, 'answer.zip');

  assert.equal(returnedBlob, blob);
  assert.equal(harness.messages.length, 1);
  assert.deepEqual(harness.messages[0], {
    type: 'intercept-page-download',
    url: responseUrl,
    filename: 'answer.zip',
    fallbackToken: 'fallback-1'
  });
  assert.equal(harness.nativeDownloads, 0);
});

test('unmapped blob downloads report an unknown source and can restore Firefox once', async () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  harness.clickDownload(objectUrl, 'unknown.bin');
  const token = harness.messages[0].fallbackToken;

  assert.equal(harness.messages[0].url, null);
  assert.deepEqual(harness.revocations, [], 'object URL stays alive until the decision');
  assert.equal(harness.createCalls.length, 2, 'fallback gets its own Blob URL');
  const firstRestore = await harness.sendRuntimeMessage({ type: 'restore-page-download', fallbackToken: token });
  const duplicateRestore = await harness.sendRuntimeMessage({ type: 'restore-page-download', fallbackToken: token });
  assert.deepEqual(firstRestore, { ok: true });
  assert.deepEqual(duplicateRestore, { ok: true });
  assert.equal(harness.nativeDownloads, 1);
  assert.equal(harness.timerCallbacks.length, 1);
  assert.equal(harness.timerCallbacks[0].delayMs, 1000);
  harness.timerCallbacks[0].callback();
  assert.deepEqual(harness.revocations, [`blob:https://chatgpt.com/fake-2`]);
});

test('intercepts a detached anchor click before Firefox handles the blob download', async () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  const detachedAnchor = harness.clickDownload(objectUrl, 'detached.bin');

  assert.equal(detachedAnchor.parentNode, null);
  assert.equal(harness.messages.length, 1);
  assert.equal(harness.messages[0].url, null);
  assert.equal(harness.nativeDownloads, 0);
});

test('a blob anchor without a download attribute keeps native navigation', () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  const anchor = harness.pageWindow.document.createElement('a');
  anchor.href = objectUrl;
  anchor.click();

  assert.equal(harness.messages.length, 0);
  assert.equal(harness.nativeDownloads, 1);
  assert.equal(harness.createCalls.length, 1);
});

test('an empty download attribute still marks a blob anchor as a download', () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  const anchor = harness.pageWindow.document.createElement('a');
  anchor.href = objectUrl;
  anchor.download = '';
  anchor.click();

  assert.equal(harness.messages.length, 1);
  assert.equal(harness.messages[0].filename, '');
  assert.equal(harness.nativeDownloads, 0);
});

test('an untracked blob URL is left to Firefox instead of consuming its click', () => {
  const harness = makeHarness();
  harness.clickDownload('blob:https://chatgpt.com/not-tracked', 'unknown.bin');

  assert.equal(harness.messages.length, 0);
  assert.equal(harness.nativeDownloads, 1);
});

test('failed fetch responses do not create a source mapping', async () => {
  const harness = makeHarness();
  const response = new harness.pageWindow.Response(
    'https://chatgpt.com/backend-api/files/denied', false, harness.makeBlob()
  );
  const blob = await response.blob();
  const objectUrl = harness.pageWindow.URL.createObjectURL(blob);
  harness.clickDownload(objectUrl);

  assert.equal(harness.messages[0].url, null);
});

test('release removes pending fallback and revokes the owned object URL', async () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  harness.clickDownload(objectUrl);
  const token = harness.messages[0].fallbackToken;

  assert.deepEqual(
    await harness.sendRuntimeMessage({ type: 'release-page-download', fallbackToken: token }),
    { ok: true }
  );
  assert.deepEqual(
    await harness.sendRuntimeMessage({ type: 'restore-page-download', fallbackToken: token }),
    { ok: false }
  );
  assert.deepEqual(harness.revocations, ['blob:https://chatgpt.com/fake-2']);
  assert.equal(harness.nativeDownloads, 0);
});

test('page revoke immediately after click does not invalidate the owned fallback URL', async () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  harness.clickDownload(objectUrl, 'immediate-revoke.bin');
  const token = harness.messages[0].fallbackToken;
  const fallbackUrl = 'blob:https://chatgpt.com/fake-2';

  harness.pageWindow.URL.revokeObjectURL(objectUrl);
  assert.deepEqual(harness.revocations, [objectUrl]);
  assert.deepEqual(
    await harness.sendRuntimeMessage({ type: 'restore-page-download', fallbackToken: token }),
    { ok: true }
  );
  assert.equal(harness.nativeDownloads, 1);
  assert.deepEqual(harness.revocations, [objectUrl]);
  harness.timerCallbacks[0].callback();
  assert.deepEqual(harness.revocations, [objectUrl, fallbackUrl]);
});

test('a background rejection of the intercept request restores Firefox', async () => {
  const harness = makeHarness({ sendMessageResponse: { ok: false } });
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  harness.clickDownload(objectUrl, 'retry.bin');
  await Promise.resolve();
  await Promise.resolve();

  assert.equal(harness.nativeDownloads, 1);
  assert.equal(harness.timerCallbacks.length, 1);
});

test('a later click using a new URL for the same Blob is intercepted after Firefox fallback', async () => {
  const responseUrl = 'https://chatgpt.com/backend-api/files/cached-response';
  const harness = makeHarness({
    sendMessageResponse: (_message, count) => count === 1 ? { ok: false } : { ok: true }
  });
  const blob = harness.makeBlob();
  const response = new harness.pageWindow.Response(responseUrl, true, blob);
  await response.blob();

  const firstUrl = harness.pageWindow.URL.createObjectURL(blob);
  harness.clickDownload(firstUrl, 'first.bin');
  await Promise.resolve();
  await Promise.resolve();
  harness.pageWindow.URL.revokeObjectURL(firstUrl);
  const secondUrl = harness.pageWindow.URL.createObjectURL(blob);
  harness.clickDownload(secondUrl, 'second.bin');

  assert.equal(harness.messages.length, 2);
  assert.equal(harness.messages[0].url, responseUrl);
  assert.equal(harness.messages[1].url, responseUrl);
  assert.notEqual(harness.messages[0].fallbackToken, harness.messages[1].fallbackToken);
  assert.equal(harness.nativeDownloads, 1, 'only the rejected first handoff falls back');
});

test('reports a failed Firefox restore instead of claiming success', async () => {
  const harness = makeHarness();
  const objectUrl = harness.pageWindow.URL.createObjectURL(harness.makeBlob());
  harness.clickDownload(objectUrl);
  const token = harness.messages[0].fallbackToken;
  harness.pageWindow.document.createElement = () => { throw new Error('document unavailable'); };

  assert.deepEqual(
    await harness.sendRuntimeMessage({ type: 'restore-page-download', fallbackToken: token }),
    { ok: false }
  );
  assert.equal(harness.nativeDownloads, 0);
});

test('unrelated runtime messages stay available to other listeners', async () => {
  const harness = makeHarness();
  assert.equal(await harness.sendRuntimeMessage({ type: 'unrelated' }), false);
});

test('dispose removes a click wrapper when the native method was inherited', () => {
  const harness = makeHarness({ inheritedAnchorClick: true });
  const anchorPrototype = harness.pageWindow.HTMLAnchorElement.prototype;
  const inheritedClick = Object.getPrototypeOf(anchorPrototype).click;
  assert.equal(Object.hasOwn(anchorPrototype, 'click'), true);

  harness.interceptor.dispose();

  assert.equal(Object.hasOwn(anchorPrototype, 'click'), false);
  assert.equal(anchorPrototype.click, inheritedClick);
});

test('wrappers preserve native Response and URL method behavior', async () => {
  const harness = makeHarness();
  const blob = harness.makeBlob();
  const response = new harness.pageWindow.Response('not a supported URL', true, blob);
  assert.equal(await response.blob(), blob);
  const objectUrl = harness.pageWindow.URL.createObjectURL(blob);
  harness.pageWindow.URL.revokeObjectURL(objectUrl);

  assert.equal(response.blobCalls, 1);
  assert.deepEqual(harness.createCalls, [blob]);
  assert.deepEqual(harness.revocations, [objectUrl]);
});
