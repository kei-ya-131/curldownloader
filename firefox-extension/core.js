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

  // Only return fixed validation labels, never a native message or header value.
  const CONTEXT_DIAGNOSTICS = [
    'too many request headers', 'cookie store id is invalid',
    'header name is empty or padded', 'header name is not an HTTP token',
    'header value contains invalid characters', 'header value is too large',
    'request context is too large', 'request context protection failed',
    'request context serialization failed',
    ...['initial URL', 'final URL', 'source page URL'].flatMap((field) =>
      ['contains invalid characters', 'is too large', 'is not a URL',
        'must be an HTTP or HTTPS URL'].map((reason) => `${field} ${reason}`))
  ];

  function nativeContextDiagnostic(error) {
    if (!error || error.code !== 'invalid_request_context' || typeof error.message !== 'string') return null;
    return CONTEXT_DIAGNOSTICS.find((reason) => error.message.includes(reason)) || null;
  }

  function requestContextDiagnostic(context) {
    if (!context) return null;
    const bytes = (value) => new TextEncoder().encode(value).length;
    if (context.headers.length > 64) return 'too many request headers';
    let total = 0;
    for (const [field, value] of [['initial URL', context.initial_url],
      ['final URL', context.final_url], ['source page URL', context.source_page_url]]) {
      if (field === 'source page URL' && value === null) continue;
      if (!value || /[\0\r\n]/.test(value)) return `${field} contains invalid characters`;
      if (bytes(value) > 48 * 1024) return `${field} is too large`;
      let url;
      try { url = new URL(value); } catch (_error) { return `${field} is not a URL`; }
      if (!['http:', 'https:'].includes(url.protocol) || !url.hostname) return `${field} must be an HTTP or HTTPS URL`;
      total += bytes(value);
    }
    const store = context.cookie_store_id;
    if (store !== null && (!store || /[\0\r\n]/.test(store) || bytes(store) > 256)) return 'cookie store id is invalid';
    for (const { name, value } of context.headers) {
      if (!name || name !== name.trim()) return 'header name is empty or padded';
      if (!/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name)) return 'header name is not an HTTP token';
      if (/[\0\r\n]/.test(value)) return 'header value contains invalid characters';
      if (bytes(value) > 48 * 1024) return 'header value is too large';
      total += bytes(name) + bytes(value);
      if (total > 48 * 1024) return 'request context is too large';
    }
    return null;
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
      // Firefox may omit container identity. Keep it absent: the native host
      // accepts null, but rejects an empty container ID.
      cookie_store_id: requestContext.cookieStoreId === undefined
        || requestContext.cookieStoreId === null || requestContext.cookieStoreId === ''
        ? null
        : String(requestContext.cookieStoreId)
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
    serializeRequestContext,
    requestContextDiagnostic,
    nativeContextDiagnostic
  };
});
