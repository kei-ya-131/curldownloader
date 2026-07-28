(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = (browserApi, options = {}) => factory(
      browserApi,
      require('./core.js'),
      require('./storage.js'),
      require('./status.js'),
      options
    );
  } else {
    root.CurlDownloaderBackground = factory(
      root.browser,
      root.CurlExtensionCore,
      root.CurlExtensionStorage,
      root.CurlExtensionStatus,
      {}
    );
  }
})(typeof globalThis === 'object' ? globalThis : this, function (browserApi, core, storage, status, runtimeOptions) {
  'use strict';
  runtimeOptions = runtimeOptions || {};
  status = status || {};
  const now = typeof runtimeOptions.now === 'function' ? runtimeOptions.now : Date.now;

  function startupFields(autoStart, startIntentUnixMs) {
    if (!autoStart) return { auto_start: false };
    const intent = Number(startIntentUnixMs);
    return {
      auto_start: true,
      start_intent_unix_ms: Number.isFinite(intent) ? intent : now()
    };
  }

  const pendingDownloads = new Map();
  const settingsTabs = new Map();
  const managedFallbackIds = new Set();
  const managedFallbackUrls = new Map();
  let nativeQueue = Promise.resolve();
  let requestSequence = 0;
  const nativeRetryOptions = {
    attempts: Number.isInteger(runtimeOptions.attempts) ? Math.max(1, runtimeOptions.attempts) : 5,
    delayMs: Number.isFinite(runtimeOptions.delayMs) ? Math.max(0, runtimeOptions.delayMs) : 2000
  };
  const badgeRefreshIntervalMs = Number.isFinite(runtimeOptions.badgeRefreshIntervalMs)
    ? Math.max(50, runtimeOptions.badgeRefreshIntervalMs)
    : 500;
  const badgeTimersEnabled = runtimeOptions.timers !== false;
  const browserActionApi = browserApi && (browserApi.browserAction || browserApi.action);
  let badgeSyncTimer = null;
  let badgeRefreshInFlight = false;
  let badgeSyncRunning = false;
  let lastBadgeSummary = null;
  let lastTaskList = [];
  let restartDelayMs = 500;
  let restartCooldownUntil = 0;

  function defaultProxy() {
    return { enabled: false, protocol: 'http', host: '', port: 8080, username: '' };
  }

  function clonePending(pending) {
    return {
      downloadId: pending.downloadId,
      url: pending.url,
      filename: pending.filename,
      targetDir: pending.targetDir || '',
      forceRecreate: Boolean(pending.forceRecreate),
      proxy: {
        enabled: Boolean(pending.proxy && pending.proxy.enabled),
        protocol: (pending.proxy && pending.proxy.protocol) || 'http',
        host: (pending.proxy && pending.proxy.host) || '',
        port: Number((pending.proxy && pending.proxy.port) || 8080),
        username: (pending.proxy && pending.proxy.username) || ''
      }
    };
  }

  function pathIsAbsolute(path) {
    return /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path);
  }

  function validateForm(form) {
    if (!form || typeof form !== 'object') return '下載設定無效';
    if (!String(form.filename || '').trim()) return '下載名稱不可為空';
    if (!pathIsAbsolute(String(form.targetDir || '').trim())) return '下載目錄必須是 Windows 絕對路徑';
    const proxy = form.proxy || {};
    if (!proxy.enabled) return null;
    if (!['http', 'https', 'socks5', 'socks5h'].includes(String(proxy.protocol))) {
      return 'Proxy 類型無效';
    }
    if (!String(proxy.host || '').trim()) return 'Proxy 主機不可為空';
    const port = Number(proxy.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) return 'Proxy 連接埠無效';
    return null;
  }

  function notifyFailure(message) {
    if (!browserApi.notifications || !browserApi.notifications.create) return Promise.resolve();
    return browserApi.notifications.create({
      type: 'basic',
      title: 'Curl Downloader',
      message: message || 'Native host 未能接收下載，已恢復 Firefox。'
    }).catch(() => undefined);
  }

  function sendNative(message) {
    const requestId = `firefox-${Date.now()}-${requestSequence++}`;
    const request = { ...message, request_id: requestId };
    const operation = nativeQueue.then(() => {
      if (!browserApi.runtime || typeof browserApi.runtime.sendNativeMessage !== 'function') {
        throw new Error('Firefox 不支援一次性 Native Messaging');
      }
      return browserApi.runtime.sendNativeMessage('curl_downloader', request);
    });
    nativeQueue = operation.catch(() => undefined);
    return operation;
  }
  async function sendNativeWithRetry(message, options = nativeRetryOptions) {
    const attempts = Number.isInteger(options.attempts)
      ? Math.max(1, options.attempts)
      : nativeRetryOptions.attempts;
    const delayMs = Number.isFinite(options.delayMs)
      ? Math.max(0, options.delayMs)
      : nativeRetryOptions.delayMs;
    let lastError;
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      try {
        return await sendNative(message);
      } catch (error) {
        lastError = error;
        if (attempt + 1 < attempts && delayMs > 0) {
          await new Promise((resolve) => setTimeout(resolve, delayMs));
        }
      }
    }
    throw lastError || new Error('Native host unavailable');
  }

  function callBrowserAction(method, details) {
    if (!browserActionApi || typeof browserActionApi[method] !== 'function') return;
    try {
      const result = browserActionApi[method](details);
      if (result && typeof result.catch === 'function') result.catch(() => undefined);
    } catch (_error) {
      // BrowserAction updates are best effort and must not interrupt downloads.
    }
  }

  function clearBadgeTimer() {
    if (badgeSyncTimer !== null) clearTimeout(badgeSyncTimer);
    badgeSyncTimer = null;
  }

  function scheduleBadgeRefresh(delayMs = badgeRefreshIntervalMs) {
    clearBadgeTimer();
    if (!badgeTimersEnabled || !badgeSyncRunning) return;
    badgeSyncTimer = setTimeout(() => {
      badgeSyncTimer = null;
      void refreshTaskStatus();
    }, Math.max(0, delayMs));
    if (typeof badgeSyncTimer.unref === 'function') badgeSyncTimer.unref();
  }

  function applyBadge(summary, warning = false) {
    if (!status.badgeState) return;
    const state = status.badgeState(summary);
    const color = warning ? '#d946ef' : state.color;
    const title = warning
      ? `${state.title}，正在重新連線`
      : state.title;
    callBrowserAction('setBadgeText', { text: state.text });
    callBrowserAction('setBadgeBackgroundColor', { color });
    callBrowserAction('setTitle', { title });
    if (status.iconDetails) {
      callBrowserAction('setIcon', status.iconDetails(state.progressStep));
    }
  }

  function applyTaskSummary(tasks) {
    const normalizedTasks = Array.isArray(tasks) ? tasks : [];
    const summary = status.summarizeTasks(normalizedTasks);
    lastTaskList = normalizedTasks;
    lastBadgeSummary = summary;
    restartCooldownUntil = 0;
    if (summary.hasActive) {
      badgeSyncRunning = true;
      restartDelayMs = 500;
      applyBadge(summary);
      scheduleBadgeRefresh();
    } else {
      badgeSyncRunning = false;
      clearBadgeTimer();
      restartDelayMs = 500;
      applyBadge(summary);
    }
    return summary;
  }

  async function refreshTaskStatus(options = {}) {
    const fromPopup = Boolean(options.fromPopup);
    if (fromPopup && restartCooldownUntil > Date.now()) {
      return lastBadgeSummary
        ? { ok: true, tasks: lastTaskList, stale: true }
        : nativeUnavailable('Curl Downloader 正在重試連線，請稍候。');
    }
    if (badgeRefreshInFlight) return { ok: true, tasks: lastTaskList };
    badgeRefreshInFlight = true;
    let failedResponse = null;
    try {
      const response = await listTasks(options);
      if (!response || !response.ok) {
        failedResponse = response;
        throw new Error(response && response.error || '無法讀取 Curl Downloader 任務。');
      }
      applyTaskSummary(response.tasks);
      return response;
    } catch (_error) {
      const retryDelay = restartDelayMs;
      restartCooldownUntil = Date.now() + retryDelay;
      if (lastBadgeSummary && lastBadgeSummary.hasActive) {
        badgeSyncRunning = true;
        applyBadge(lastBadgeSummary, true);
        scheduleBadgeRefresh(retryDelay);
        restartDelayMs = Math.min(30000, restartDelayMs * 2);
      } else {
        badgeSyncRunning = false;
        clearBadgeTimer();
        applyBadge({ activeCount: 0, hasFailure: false, hasProxyPassword: false });
        restartDelayMs = Math.min(30000, restartDelayMs * 2);
      }
      return failedResponse || nativeUnavailable('Curl Downloader 未啟動或尚未註冊 Native host，無法讀取任務。');
    } finally {
      badgeRefreshInFlight = false;
    }
  }
  function nativeUnavailable(error) {
    return {
      ok: false,
      code: 'native_unavailable',
      error: error || 'Curl Downloader 未啟動或尚未註冊 Native host。'
    };
  }

  function consumeManagedFallback(download) {
    if (managedFallbackIds.delete(download.id)) return true;
    const count = managedFallbackUrls.get(download.url) || 0;
    if (count > 0) {
      if (count === 1) managedFallbackUrls.delete(download.url);
      else managedFallbackUrls.set(download.url, count - 1);
      return true;
    }
    return false;
  }

  function markManagedFallbackUrl(url) {
    managedFallbackUrls.set(url, (managedFallbackUrls.get(url) || 0) + 1);
  }

  function unmarkManagedFallbackUrl(url) {
    const count = managedFallbackUrls.get(url) || 0;
    if (count <= 1) managedFallbackUrls.delete(url);
    else managedFallbackUrls.set(url, count - 1);
  }

  async function restoreFirefoxDownload(pending) {
    if (!pending.forceRecreate) {
      try {
        await browserApi.downloads.resume(pending.downloadId);
        return { ok: true, recreated: false };
      } catch (_error) {
        // The item may have disappeared or Firefox may reject resume after a pause failure.
      }
    }

    markManagedFallbackUrl(pending.url);
    try {
      const id = await browserApi.downloads.download({
        url: pending.url,
        filename: core.fallbackFilename(pending.filename),
        saveAs: false,
        conflictAction: 'uniquify'
      });
      if (managedFallbackUrls.get(pending.url)) managedFallbackIds.add(id);
      return { ok: true, recreated: true, id };
    } catch (_error) {
      unmarkManagedFallbackUrl(pending.url);
      return { ok: false, recreated: false };
    }
  }

  async function cancelFirefoxDownload(pending) {
    try {
      await browserApi.downloads.cancel(pending.downloadId);
    } catch (_error) {
      return { ok: false, error: 'Firefox 下載未能取消。' };
    }
    try {
      await browserApi.downloads.erase({ id: pending.downloadId });
    } catch (_error) {
      // Cancellation succeeded; failure to erase history must not keep the task pending.
    }
    return { ok: true };
  }

  async function handleCreatedDownload(download) {
    if (consumeManagedFallback(download)) return { ignored: true };
    if (!core.isSupportedDownloadUrl(download.url)) return { ignored: true };
    if (pendingDownloads.has(download.id)) return { ignored: true };

    let defaults = { targetDir: '', proxy: defaultProxy() };
    try {
      defaults = await storage.loadDefaults();
    } catch (_error) {
      // Settings can still be shown with empty defaults if storage is unavailable.
    }
    const pending = {
      downloadId: download.id,
      url: String(download.url),
      filename: core.fallbackFilename(download.filename || 'download.bin'),
      targetDir: defaults.targetDir || '',
      proxy: defaults.proxy || defaultProxy(),
      forceRecreate: false
    };
    pendingDownloads.set(download.id, pending);

    try {
      await browserApi.downloads.pause(download.id);
    } catch (_error) {
      pending.forceRecreate = true;
      await restoreFirefoxDownload(pending);
      pendingDownloads.delete(download.id);
      return { restored: true };
    }

    try {
      const getUrl = browserApi.runtime.getURL
        ? browserApi.runtime.getURL('settings.html')
        : 'settings.html';
      const tab = await browserApi.tabs.create({
        url: `${getUrl}?downloadId=${encodeURIComponent(download.id)}`
      });
      if (tab && tab.id !== undefined) settingsTabs.set(tab.id, download.id);
      return { paused: true, tabId: tab && tab.id };
    } catch (_error) {
      await restoreFirefoxDownload(pending);
      pendingDownloads.delete(download.id);
      return { restored: true };
    }
  }

  async function submitExternalDownload(downloadId, form, startIntentUnixMs) {
    const pending = pendingDownloads.get(Number(downloadId));
    if (!pending) return { ok: false, error: '找不到暫停中的下載。' };
    const validationError = validateForm(form);
    if (validationError) return { ok: false, error: validationError };

    const request = {
      ...core.buildEnqueueMessage(pending, form, 'pending'),
      ...startupFields(true, startIntentUnixMs)
    };
    let accepted = false;
    try {
      const response = await sendNativeWithRetry(request);
      if (!response || response.type !== 'enqueue_result' || !response.ok) {
        throw new Error('Curl Downloader 未接收任務');
      }
      accepted = true;
      try {
        await browserApi.downloads.cancel(pending.downloadId);
        await browserApi.downloads.erase({ id: pending.downloadId });
      } catch (_error) {
        await notifyFailure('Curl Downloader 已接收任務，但 Firefox 原下載未能清理。');
      }
      pendingDownloads.delete(pending.downloadId);
      restartDelayMs = 500;
      restartCooldownUntil = 0;
      void refreshTaskStatus();
      try {
        await storage.saveDefaults(form);
      } catch (_error) {
        // Saving defaults must never change the already accepted download.
      }
      return { ok: true, taskId: response.task_id };
    } catch (_error) {
      if (!accepted) {
        await notifyFailure('Native host 未能接收下載，已恢復 Firefox。');
        await restoreFirefoxDownload(pending);
        pendingDownloads.delete(pending.downloadId);
      }
      return { ok: false, code: 'native_unavailable', error: 'Native host 未能接收下載，已恢復 Firefox。' };
    }
  }

  function validTaskId(value) {
    const taskId = Number(value);
    return Number.isSafeInteger(taskId) && taskId >= 0 ? taskId : null;
  }

  async function listTasks(options = {}) {
    try {
      const response = await sendNativeWithRetry({
        type: 'list_tasks',
        ...startupFields(Boolean(options.autoStart), options.startIntentUnixMs)
      });
      if (!response || response.type !== 'task_list' || !Array.isArray(response.tasks)) {
        return { ok: false, error: '無法讀取 Curl Downloader 任務。' };
      }
      return { ok: true, tasks: response.tasks };
    } catch (_error) {
      return nativeUnavailable('Curl Downloader 未啟動或尚未註冊 Native host，無法讀取任務。');
    }
  }

  async function sendTaskAction(message) {
    const taskId = validTaskId(message.taskId);
    if (taskId === null) return { ok: false, error: '任務編號無效。' };
    const nativeType = {
      'show-task': 'show_task',
      'open-file': 'open_file',
      'open-folder': 'open_folder'
    }[message.type];
    try {
      const response = await sendNativeWithRetry({ type: nativeType, task_id: taskId, ...startupFields(true, message.startIntentUnixMs) });
      if (!response || response.type !== 'action_result') {
        return { ok: false, error: 'Curl Downloader 未能完成操作。' };
      }
      if (!response.ok) {
        return {
          ok: false,
          error: response.error && response.error.message
            ? response.error.message
            : 'Curl Downloader 未能完成操作。'
        };
      }
      return { ok: true };
    } catch (_error) {
      return nativeUnavailable('Curl Downloader 未啟動或尚未註冊 Native host。');
    }
  }

  async function handleRuntimeMessage(message) {
    if (!message || typeof message !== 'object') return { ok: false, error: '訊息無效' };
    const id = Number(message.downloadId);
    if (message.type === 'get-pending') {
      const pending = pendingDownloads.get(id);
      return pending ? { ok: true, download: clonePending(pending) } : { ok: false, error: '找不到下載項目。' };
    }
    if (message.type === 'submit-external') return submitExternalDownload(id, message.form, message.startIntentUnixMs);
    if (message.type === 'restore-firefox' || message.type === 'fallback') {
      const pending = pendingDownloads.get(id);
      if (!pending) return { ok: false, error: '找不到下載項目。' };
      const result = await restoreFirefoxDownload(pending);
      pendingDownloads.delete(id);
      return result;
    }
    if (message.type === 'cancel-download') {
      const pending = pendingDownloads.get(id);
      if (!pending) return { ok: false, error: '找不到下載項目。' };
      const result = await cancelFirefoxDownload(pending);
      if (result.ok) pendingDownloads.delete(id);
      return result;
    }
    if (message.type === 'pick-folder') {
      try {
        const response = await sendNativeWithRetry({ type: 'pick_folder', ...startupFields(true, message.startIntentUnixMs) });
        if (response && response.type === 'folder') {
          return {
            ok: Boolean(response.ok),
            targetDir: response.target_dir || '',
            error: response.error && response.error.message ? response.error.message : null
          };
        }
        return { ok: false, error: '無法開啟目錄選擇器。' };
      } catch (_error) {
        return nativeUnavailable('Curl Downloader 未啟動或尚未註冊 Native host，無法開啟目錄選擇器。');
      }
    }
    if (message.type === 'get-task-summary') return refreshTaskStatus({ fromPopup: true, autoStart: Boolean(message.autoStart), startIntentUnixMs: message.startIntentUnixMs });
    if (message.type === 'show-task' || message.type === 'open-file' || message.type === 'open-folder') {
      return sendTaskAction(message);
    }
    if (message.type === 'get-defaults') {
      try {
        const response = await sendNativeWithRetry({ type: 'get_defaults', ...startupFields(true, message.startIntentUnixMs) });
        return response && response.type === 'defaults'
          ? { ok: true, targetDir: response.target_dir || '' }
          : { ok: false, error: '無法讀取 Curl Downloader 預設目錄。' };
      } catch (_error) {
        return nativeUnavailable();
      }
    }
    return { ok: false, error: '未知訊息類型' };
  }

  if (browserApi && browserApi.downloads && browserApi.downloads.onCreated) {
    browserApi.downloads.onCreated.addListener((download) => { void handleCreatedDownload(download); });
  }
  if (browserApi && browserApi.tabs && browserApi.tabs.onRemoved) {
    browserApi.tabs.onRemoved.addListener((tabId) => {
      const downloadId = settingsTabs.get(tabId);
      if (downloadId === undefined) return;
      settingsTabs.delete(tabId);
      const pending = pendingDownloads.get(downloadId);
      if (pending) {
        void restoreFirefoxDownload(pending).finally(() => pendingDownloads.delete(downloadId));
      }
    });
  }
  if (browserApi && browserApi.runtime && browserApi.runtime.onMessage) {
    browserApi.runtime.onMessage.addListener(handleRuntimeMessage);
  }

  return {
    handleCreatedDownload,
    restoreFirefoxDownload,
    submitExternalDownload,
    handleRuntimeMessage,
    sendNativeWithRetry,
    refreshTaskStatus,
    isBadgeSyncRunning: () => badgeSyncRunning
  };
});
