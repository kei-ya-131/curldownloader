(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory;
  } else if (root && root.window && root.browser) {
    factory(root.window, root.browser, { exportFunction: root.exportFunction });
  }
})(typeof globalThis === 'object' ? globalThis : this, function installBlobDownloadInterceptor(contentWindow, browserApi, suppliedOptions) {
  'use strict';

  const options = suppliedOptions || {};

  const pageWindow = contentWindow && contentWindow.wrappedJSObject;
  const runtime = browserApi && browserApi.runtime;
  const exportToPage = options.exportFunction;
  if (!pageWindow || typeof exportToPage !== 'function' || !runtime
    || typeof runtime.sendMessage !== 'function' || !runtime.onMessage
    || typeof runtime.onMessage.addListener !== 'function') {
    return null;
  }

  const responsePrototype = pageWindow.Response && pageWindow.Response.prototype;
  const pageUrl = pageWindow.URL;
  const anchorPrototype = pageWindow.HTMLAnchorElement && pageWindow.HTMLAnchorElement.prototype;
  const document = contentWindow.document;
  if (!responsePrototype || typeof responsePrototype.blob !== 'function' || !pageUrl || !anchorPrototype
    || typeof anchorPrototype.click !== 'function'
    || typeof pageUrl.createObjectURL !== 'function' || typeof pageUrl.revokeObjectURL !== 'function'
    || !document || typeof document.addEventListener !== 'function') {
    return null;
  }

  const maximumMappings = Number.isInteger(options.maximumMappings)
    ? Math.max(1, options.maximumMappings)
    : 128;
  const responseBlobDescriptor = Object.getOwnPropertyDescriptor(responsePrototype, 'blob');
  const createObjectURLDescriptor = Object.getOwnPropertyDescriptor(pageUrl, 'createObjectURL');
  const revokeObjectURLDescriptor = Object.getOwnPropertyDescriptor(pageUrl, 'revokeObjectURL');
  const anchorClickDescriptor = Object.getOwnPropertyDescriptor(anchorPrototype, 'click');
  const originalBlob = responsePrototype.blob;
  const originalCreateObjectURL = pageUrl.createObjectURL;
  const originalRevokeObjectURL = pageUrl.revokeObjectURL;
  const originalAnchorClick = anchorPrototype.click;
  const blobSources = new WeakMap();
  const objectSources = new Map();
  const pendingFallbacks = new Map();
  const bypassClicks = new WeakSet();
  let tokenSequence = 0;

  function restoreDescriptor(target, property, descriptor) {
    if (descriptor) Object.defineProperty(target, property, descriptor);
    else delete target[property];
  }

  function unwrap(object) {
    if (!object || (typeof object !== 'object' && typeof object !== 'function')) return object;
    try {
      return object.wrappedJSObject || object;
    } catch (_error) {
      return object;
    }
  }

  function responseValue(response, property) {
    const descriptor = Object.getOwnPropertyDescriptor(responsePrototype, property);
    if (descriptor && typeof descriptor.get === 'function') {
      return Reflect.apply(descriptor.get, response, []);
    }
    return response[property];
  }

  function supportedSource(value) {
    try {
      const serializedUrl = String(value);
      const source = new URL(serializedUrl);
      return (source.protocol === 'http:' || source.protocol === 'https:') ? serializedUrl : null;
    } catch (_error) {
      return null;
    }
  }

  function rememberObjectUrl(url, sourceUrl, blob) {
    if (objectSources.has(url)) objectSources.delete(url);
    while (objectSources.size >= maximumMappings) {
      objectSources.delete(objectSources.keys().next().value);
    }
    objectSources.set(url, { sourceUrl, blob });
  }

  function randomToken() {
    if (typeof options.randomToken === 'function') return String(options.randomToken());
    const cryptoApi = contentWindow.crypto;
    if (cryptoApi && typeof cryptoApi.getRandomValues === 'function') {
      const bytes = new Uint8Array(16);
      cryptoApi.getRandomValues(bytes);
      return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
    }
    tokenSequence += 1;
    return `${Date.now().toString(36)}-${tokenSequence.toString(36)}-${Math.random().toString(36).slice(2)}`;
  }

  function releaseFallback(token) {
    const fallback = pendingFallbacks.get(token);
    if (!fallback) return false;
    pendingFallbacks.delete(token);
    try {
      Reflect.apply(originalRevokeObjectURL, pageUrl, [fallback.objectUrl]);
    } catch (_error) {
      // The page may already have invalidated its object URL.
    }
    return true;
  }

  function restoreFallback(token) {
    const fallback = pendingFallbacks.get(token);
    if (!fallback) return false;
    if (fallback.decision !== 'pending') return fallback.decision === 'restored';
    fallback.decision = 'restored';
    try {
      const anchor = document.createElement('a');
      anchor.href = fallback.objectUrl;
      anchor.download = fallback.filename;
      if (anchor.style) anchor.style.display = 'none';
      const parent = document.body || document.documentElement;
      if (parent && typeof parent.appendChild === 'function') parent.appendChild(anchor);
      bypassClicks.add(anchor);
      anchor.click();
      if (anchor.parentNode && typeof anchor.parentNode.removeChild === 'function') {
        anchor.parentNode.removeChild(anchor);
      }
    } catch (_error) {
      // Keep Firefox's original download path best effort if DOM activation fails.
      fallback.decision = 'pending';
      return false;
    }
    const schedule = typeof options.setTimeout === 'function' ? options.setTimeout : setTimeout;
    schedule(() => releaseFallback(token), 1000);
    return true;
  }

  function onRuntimeMessage(message) {
    if (!message || typeof message.fallbackToken !== 'string') return false;
    if (message.type === 'restore-page-download') {
      return Promise.resolve({ ok: restoreFallback(message.fallbackToken) });
    }
    if (message.type === 'release-page-download') {
      return Promise.resolve({ ok: releaseFallback(message.fallbackToken) });
    }
    return false;
  }

  function findAnchor(target) {
    for (let node = target; node; node = node.parentNode) {
      if (String(node.tagName || '').toLowerCase() === 'a') return node;
    }
    return null;
  }

  function interceptAnchor(anchor, event) {
    let objectUrl;
    try {
      objectUrl = String(anchor.href || '');
    } catch (_error) {
      return false;
    }
    const hasDownloadAttribute = anchor.download !== ''
      || (typeof anchor.hasAttribute === 'function' && anchor.hasAttribute('download'));
    if (!objectUrl.startsWith('blob:') || !hasDownloadAttribute
      || pendingFallbacks.size >= maximumMappings) return false;

    const mappedObject = objectSources.get(objectUrl);
    if (!mappedObject || !mappedObject.blob) return false;
    let fallbackUrl;
    try {
      fallbackUrl = Reflect.apply(originalCreateObjectURL, pageUrl, [mappedObject.blob]);
    } catch (_error) {
      return false;
    }
    if (typeof fallbackUrl !== 'string' || !fallbackUrl.startsWith('blob:')) return false;

    const fallbackToken = randomToken();
    const filename = String(anchor.download || '');
    const sourceUrl = mappedObject.sourceUrl;
    pendingFallbacks.set(fallbackToken, {
      objectUrl: fallbackUrl,
      filename,
      decision: 'pending'
    });
    if (event && typeof event.preventDefault === 'function') event.preventDefault();

    try {
      Promise.resolve(runtime.sendMessage({
        type: 'intercept-page-download',
        url: sourceUrl,
        filename,
        fallbackToken
      })).then((response) => {
        if (!response || response.ok !== true) restoreFallback(fallbackToken);
      }, () => restoreFallback(fallbackToken));
    } catch (_error) {
      restoreFallback(fallbackToken);
    }
    return true;
  }

  function onClick(event) {
    const anchor = findAnchor(event && event.target);
    if (!anchor) return;
    if (bypassClicks.has(anchor)) {
      bypassClicks.delete(anchor);
      return;
    }
    interceptAnchor(anchor, event);
  }

  const exportedBlob = exportToPage(function (...args) {
    let sourceUrl = null;
    try {
      if (responseValue(this, 'ok') === true) sourceUrl = supportedSource(responseValue(this, 'url'));
    } catch (_error) {
      sourceUrl = null;
    }
    const result = Reflect.apply(originalBlob, this, args);
    if (!sourceUrl || !result || typeof result.then !== 'function') return result;
    const onBlob = exportToPage(function (blob) {
      const key = unwrap(blob);
      if (key && (typeof key === 'object' || typeof key === 'function')) blobSources.set(key, sourceUrl);
      return blob;
    }, pageWindow);
    return result.then(onBlob);
  }, pageWindow);

  const exportedCreateObjectURL = exportToPage(function (blob, ...args) {
    const url = Reflect.apply(originalCreateObjectURL, pageUrl, [blob, ...args]);
    const key = unwrap(blob);
    const sourceUrl = key && blobSources.get(key);
    if (key && typeof url === 'string' && url.startsWith('blob:')) {
      rememberObjectUrl(url, sourceUrl || null, key);
    }
    return url;
  }, pageWindow);

  const exportedRevokeObjectURL = exportToPage(function (url) {
    const objectUrl = String(url);
    objectSources.delete(objectUrl);
    return Reflect.apply(originalRevokeObjectURL, pageUrl, [url]);
  }, pageWindow);

  const exportedAnchorClick = exportToPage(function (...args) {
    if (bypassClicks.has(this)) return Reflect.apply(originalAnchorClick, this, args);
    if (interceptAnchor(this, null)) return undefined;
    return Reflect.apply(originalAnchorClick, this, args);
  }, pageWindow);

  try {
    Object.defineProperty(responsePrototype, 'blob', {
      ...(responseBlobDescriptor || { configurable: true, writable: true }),
      value: exportedBlob
    });
    Object.defineProperty(pageUrl, 'createObjectURL', {
      ...(createObjectURLDescriptor || { configurable: true, writable: true }),
      value: exportedCreateObjectURL
    });
    Object.defineProperty(pageUrl, 'revokeObjectURL', {
      ...(revokeObjectURLDescriptor || { configurable: true, writable: true }),
      value: exportedRevokeObjectURL
    });
    Object.defineProperty(anchorPrototype, 'click', {
      ...(anchorClickDescriptor || { configurable: true, writable: true }),
      value: exportedAnchorClick
    });
  } catch (_error) {
    restoreDescriptor(responsePrototype, 'blob', responseBlobDescriptor);
    restoreDescriptor(pageUrl, 'createObjectURL', createObjectURLDescriptor);
    restoreDescriptor(pageUrl, 'revokeObjectURL', revokeObjectURLDescriptor);
    restoreDescriptor(anchorPrototype, 'click', anchorClickDescriptor);
    return null;
  }

  document.addEventListener('click', onClick, true);
  runtime.onMessage.addListener(onRuntimeMessage);
  return {
    dispose() {
      document.removeEventListener('click', onClick, true);
      if (runtime.onMessage.removeListener) runtime.onMessage.removeListener(onRuntimeMessage);
      restoreDescriptor(responsePrototype, 'blob', responseBlobDescriptor);
      restoreDescriptor(pageUrl, 'createObjectURL', createObjectURLDescriptor);
      restoreDescriptor(pageUrl, 'revokeObjectURL', revokeObjectURLDescriptor);
      restoreDescriptor(anchorPrototype, 'click', anchorClickDescriptor);
      for (const token of [...pendingFallbacks.keys()]) releaseFallback(token);
      objectSources.clear();
    }
  };
});
