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

  function buildEnqueueMessage(download, form, requestId) {
    const proxy = form.proxy || {};
    return {
      type: 'enqueue',
      request_id: String(requestId),
      url: String(download.url),
      filename: String(form.filename || download.filename || '').trim(),
      target_dir: String(form.targetDir || '').trim(),
      proxy: {
        enabled: Boolean(proxy.enabled),
        protocol: String(proxy.protocol || 'http'),
        host: String(proxy.host || '').trim(),
        port: Number(proxy.port) || 8080,
        username: String(proxy.username || ''),
        password: String(proxy.password || '')
      }
    };
  }

  return {
    isSupportedDownloadUrl,
    fallbackFilename,
    buildEnqueueMessage
  };
});
