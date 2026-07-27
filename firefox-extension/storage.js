(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory(null);
  } else {
    root.CurlExtensionStorage = factory(root.browser);
  }
})(typeof globalThis === 'object' ? globalThis : this, function (browserApi) {
  'use strict';

  const DEFAULTS = Object.freeze({
    targetDir: '',
    proxy: Object.freeze({
      enabled: false,
      protocol: 'http',
      host: '',
      port: 8080,
      username: ''
    })
  });

  function cleanDefaults(value) {
    const source = value && typeof value === 'object' ? value : {};
    const sourceProxy = source.proxy && typeof source.proxy === 'object' ? source.proxy : {};
    return {
      targetDir: typeof source.targetDir === 'string' ? source.targetDir : '',
      proxy: {
        enabled: Boolean(sourceProxy.enabled),
        protocol: String(sourceProxy.protocol || DEFAULTS.proxy.protocol),
        host: typeof sourceProxy.host === 'string' ? sourceProxy.host : '',
        port: Number(sourceProxy.port) || DEFAULTS.proxy.port,
        username: typeof sourceProxy.username === 'string' ? sourceProxy.username : ''
      }
    };
  }

  async function loadDefaults() {
    if (!browserApi || !browserApi.storage || !browserApi.storage.local) {
      return cleanDefaults(DEFAULTS);
    }
    const stored = await browserApi.storage.local.get('curlDownloaderDefaults');
    return cleanDefaults(stored.curlDownloaderDefaults);
  }

  async function saveDefaults(defaults) {
    const cleaned = cleanDefaults(defaults);
    if (browserApi && browserApi.storage && browserApi.storage.local) {
      await browserApi.storage.local.set({ curlDownloaderDefaults: cleaned });
    }
    return cleaned;
  }

  return { loadDefaults, saveDefaults, cleanDefaults };
});
