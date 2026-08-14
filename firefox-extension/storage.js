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
    segments: 4,
    proxy: Object.freeze({
      enabled: false,
      protocol: 'http',
      host: '',
      port: 8080,
      username: ''
    })
  });
  const TASK_BINDINGS_KEY = 'curlDownloaderFirefoxTaskBindings';

  function cleanSegments(value) {
    const segments = Number(value);
    return Number.isInteger(segments) && segments >= 1 && segments <= 8
      ? segments
      : DEFAULTS.segments;
  }

  function cleanDefaults(value) {
    const source = value && typeof value === 'object' ? value : {};
    const sourceProxy = source.proxy && typeof source.proxy === 'object' ? source.proxy : {};
    return {
      targetDir: typeof source.targetDir === 'string' ? source.targetDir : '',
      segments: cleanSegments(source.segments),
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

  function cleanTaskBinding(value) {
    const source = value && typeof value === 'object' ? value : {};
    const tabId = Number(source.tabId);
    return {
      sourcePageUrl: typeof source.sourcePageUrl === 'string' ? source.sourcePageUrl : '',
      sourceOrigin: typeof source.sourceOrigin === 'string' ? source.sourceOrigin : '',
      resourceUrl: typeof source.resourceUrl === 'string' ? source.resourceUrl : '',
      tabId: Number.isInteger(tabId) && tabId >= 0 ? tabId : null,
      incognito: Boolean(source.incognito),
      cookieStoreId: source.cookieStoreId === undefined || source.cookieStoreId === null
        ? null
        : String(source.cookieStoreId)
    };
  }

  async function loadTaskBinding(taskId) {
    if (!browserApi || !browserApi.storage || !browserApi.storage.local) return null;
    const stored = await browserApi.storage.local.get(TASK_BINDINGS_KEY);
    const bindings = stored && stored[TASK_BINDINGS_KEY] && typeof stored[TASK_BINDINGS_KEY] === 'object'
      ? stored[TASK_BINDINGS_KEY]
      : {};
    const binding = bindings[String(taskId)];
    return binding ? cleanTaskBinding(binding) : null;
  }

  async function saveTaskBinding(taskId, binding) {
    const cleaned = cleanTaskBinding(binding);
    if (!browserApi || !browserApi.storage || !browserApi.storage.local) return cleaned;
    const stored = await browserApi.storage.local.get(TASK_BINDINGS_KEY);
    const bindings = stored && stored[TASK_BINDINGS_KEY] && typeof stored[TASK_BINDINGS_KEY] === 'object'
      ? { ...stored[TASK_BINDINGS_KEY] }
      : {};
    bindings[String(taskId)] = cleaned;
    await browserApi.storage.local.set({ [TASK_BINDINGS_KEY]: bindings });
    return cleaned;
  }

  async function removeTaskBinding(taskId) {
    if (!browserApi || !browserApi.storage || !browserApi.storage.local) return;
    const stored = await browserApi.storage.local.get(TASK_BINDINGS_KEY);
    const bindings = stored && stored[TASK_BINDINGS_KEY] && typeof stored[TASK_BINDINGS_KEY] === 'object'
      ? { ...stored[TASK_BINDINGS_KEY] }
      : {};
    delete bindings[String(taskId)];
    await browserApi.storage.local.set({ [TASK_BINDINGS_KEY]: bindings });
  }

  return {
    loadDefaults,
    saveDefaults,
    cleanDefaults,
    cleanSegments,
    cleanTaskBinding,
    loadTaskBinding,
    saveTaskBinding,
    removeTaskBinding
  };
});
