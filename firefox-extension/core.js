(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory();
  } else {
    root.CurlExtensionCore = factory();
  }
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';

  function isSupportedDownloadUrl(url) {
    try {
      const protocol = new URL(String(url)).protocol;
      return protocol === 'http:' || protocol === 'https:';
    } catch (_error) {
      return false;
    }
  }

  function fallbackFilename(filename) {
    const normalized = String(filename || '').replace(/[\\/]+/g, '/');
    const basename = normalized.split('/').pop().trim();
    if (!basename || basename === '.' || basename === '..') {
      return 'download.bin';
    }
    return basename;
  }

  function serializeRequestContext(requestContext, download) {
    if (!requestContext || typeof requestContext !== 'object') return null;
    const headers = Array.isArray(requestContext.headers)
      ? requestContext.headers
        .filter((header) => header && typeof header === 'object')
        .map((header) => ({
          name: String(header.name || ''),
          value: String(header.value === undefined || header.value === null ? '' : header.value)
        }))
      : [];
    return {
      headers,
      source_page_url: requestContext.sourcePageUrl || download.referrer || null,
      initial_url: String(requestContext.initialUrl || download.url),
      final_url: String(requestContext.finalUrl || download.url),
      incognito: Boolean(requestContext.incognito),
      cookie_store_id: requestContext.cookieStoreId === undefined
        ? null
        : String(requestContext.cookieStoreId || '')
    };
  }

  function buildEnqueueMessage(download, form, requestId, requestContext) {
    const proxy = form.proxy || {};
    const segments = Number(form.segments);
    const message = {
      type: 'enqueue',
      request_id: String(requestId),
      url: String(download.url),
      filename: String(form.filename || download.filename || '').trim(),
      target_dir: String(form.targetDir || '').trim(),
      requested_segments: Number.isInteger(segments) && segments >= 1 && segments <= 8 ? segments : 4,
      proxy: {
        enabled: Boolean(proxy.enabled),
        protocol: String(proxy.protocol || 'http'),
        host: String(proxy.host || '').trim(),
        port: Number(proxy.port) || 8080,
        username: String(proxy.username || ''),
        password: String(proxy.password || '')
      }
    };
    const serializedContext = serializeRequestContext(requestContext, download);
    if (serializedContext) message.request_context = serializedContext;
    return message;
  }

  return {
    isSupportedDownloadUrl,
    fallbackFilename,
    buildEnqueueMessage,
    serializeRequestContext
  };
});
