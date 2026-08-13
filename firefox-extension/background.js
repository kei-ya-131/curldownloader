(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = (browserApi, options = {}) => factory(
      browserApi,
      require('./core.js'),
      require('./storage.js'),
      require('./status.js'),
      require('./native-session.js'),
      options
    );
  } else {
    root.CurlDownloaderBackground = factory(
      root.browser,
      root.CurlExtensionCore,
      root.CurlExtensionStorage,
      root.CurlExtensionStatus,
      root.CurlDownloaderNativeSession,
      {}
    );
  }
})(typeof globalThis === 'object' ? globalThis : this, function (browserApi, core, storage, status, nativeSessionFactory, runtimeOptions) {
  'use strict';
  runtimeOptions = runtimeOptions || {};
  status = status || {};
  const now = typeof runtimeOptions.now === 'function' ? runtimeOptions.now : Date.now;
  const nativeSession = runtimeOptions.nativeSession || (
    typeof nativeSessionFactory === 'function'
      ? nativeSessionFactory(browserApi, { idleMs: runtimeOptions.nativeIdleMs })
      : null
  );

  function startupFields(autoStart, startIntentUnixMs) {
    if (!autoStart) return { auto_start: false };
    const intent = Number(startIntentUnixMs);
    return {
      auto_start: true,
      start_intent_unix_ms: Number.isFinite(intent) ? intent : now()
    };
  }

  function passiveStartupFields() {
    return { auto_start: true };
  }

  const pendingDownloads = new Map();
  const settingsTabs = new Map();
  const managedFallbackIds = new Set();
  const managedFallbackUrls = new Map();
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
  let popupOpen = false;

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
      firefoxDownloadRemoved: Boolean(pending.firefoxDownloadRemoved),
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
    const segments = form.segments === undefined ? 4 : Number(form.segments);
    if (!Number.isInteger(segments)) return '下載線程數量必須是整數';
    if (segments < 1 || segments > 8) return '下載線程數量必須介乎 1 至 8';
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
    if (!nativeSession) throw new Error('Firefox 不支援持續 Native Messaging');
    return nativeSession.send(message);
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

  function updateNativeKeepAlive(hasActive = Boolean(lastBadgeSummary && lastBadgeSummary.hasActive)) {
    if (nativeSession && typeof nativeSession.setKeepAlive === 'function') {
      nativeSession.setKeepAlive(popupOpen || Boolean(hasActive));
    }
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
    updateNativeKeepAlive(summary.hasActive);
    return summary;
  }

  async function refreshTaskStatus(options = {}) {
    const fromPopup = Boolean(options.fromPopup);
    const startIntentUnixMs = Number(options.startIntentUnixMs);
    const hasExplicitStartIntent = Number.isFinite(startIntentUnixMs) && startIntentUnixMs > 0;
    if (fromPopup && !hasExplicitStartIntent && restartCooldownUntil > now()) {
      return lastBadgeSummary
        ? { ok: true, tasks: lastTaskList, stale: true }
        : nativeUnavailable('Curl Downloader 正在重試連線，請稍候。');
    }
    if (badgeRefreshInFlight) return { ok: true, tasks: lastTaskList };
    badgeRefreshInFlight = true;
    let failedResponse = null;
    try {
      const response = await listTasks({ startIntentUnixMs });
      if (!response || !response.ok) {
        failedResponse = response;
        throw new Error(response && response.error || '無法讀取 Curl Downloader 任務。');
      }
      applyTaskSummary(response.tasks);
      return response;
    } catch (_error) {
      if (failedResponse && failedResponse.code === 'manually_stopped') {
        badgeSyncRunning = false;
        clearBadgeTimer();
        applyBadge({ activeCount: 0, hasFailure: false, hasProxyPassword: false });
        updateNativeKeepAlive(false);
        if (nativeSession && typeof nativeSession.close === 'function') {
          nativeSession.close('Curl Downloader manually stopped');
        }
        return failedResponse;
      }
      const retryDelay = restartDelayMs;
      restartCooldownUntil = now() + retryDelay;
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
    if (pending.firefoxDownloadRemoved === false && pending.forceRecreate) {
      const removed = await cancelAndEraseDownload(pending);
      if (!removed) {
        pending.forceRecreate = false;
      }
    }
    if (!pending.forceRecreate) {
      try {
        await browserApi.downloads.resume(pending.downloadId);
        return { ok: true, recreated: false };
      } catch (_error) {
        // The item may have disappeared or Firefox may reject resume after a pause failure.
        if (pending.firefoxDownloadRemoved === false) {
          return { ok: false, recreated: false };
        }
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

  async function restorePendingAfterTabClose(pending) {
    if (pending.acceptedTaskId !== undefined) {
      const cancelled = await cancelPendingCurlTask(pending);
      if (!cancelled) {
        await notifyFailure('Curl Downloader 任務尚未取消；為避免重複下載，請重新開啟設定頁重試。');
        await openRetrySettingsTab(pending.downloadId);
        return { ok: false, error: 'Curl Downloader 任務尚未取消。' };
      }
    }
    return restoreFirefoxDownload(pending);
  }

  async function restoreAfterInterceptionFailure(pending) {
    if (pending.firefoxDownloadRemoved) {
      pending.forceRecreate = true;
      return restoreFirefoxDownload(pending);
    }
    const removed = await cancelAndEraseDownload(pending);
    if (removed) {
      pending.forceRecreate = true;
      return restoreFirefoxDownload(pending);
    }
    pending.forceRecreate = false;
    return restoreFirefoxDownload(pending);
  }

  // Firefox displays its native download panel as soon as onCreated fires.
  // Remove the original item immediately; a Firefox fallback is recreated
  // only when the user explicitly chooses it.
  async function cancelAndEraseDownload(pending) {
    let cancelled = true;
    try {
      await browserApi.downloads.cancel(pending.downloadId);
    } catch (_error) {
      cancelled = false;
    }
    if (!cancelled) return false;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      try {
        await browserApi.downloads.erase({ id: pending.downloadId });
        pending.firefoxDownloadRemoved = true;
        return true;
      } catch (_error) {
        // Firefox can finish cancelling the item just after cancel() resolves.
        // A few immediate retries avoid reopening the native download panel.
        if (attempt < 2) await Promise.resolve();
      }
    }
    return false;
  }

  async function cancelFirefoxDownload(pending) {
    if (pending.firefoxDownloadRemoved) {
      pendingDownloads.delete(pending.downloadId);
      return { ok: true };
    }
    try {
      await browserApi.downloads.cancel(pending.downloadId);
    } catch (_error) {
      return { ok: false, error: 'Firefox 下載未能取消。' };
    }
    try {
      await browserApi.downloads.erase({ id: pending.downloadId });
    } catch (_error) {
      return { ok: false, error: 'Firefox 下載已取消，但未能隱藏下載項目。' };
    }
    return { ok: true };
  }

  async function restoreAndReport(pending) {
    const result = await restoreFirefoxDownload(pending);
    if (!result.ok) {
      return {
        ok: false,
        error: 'Firefox 下載未能恢復；請保留此設定頁並稍後重試。'
      };
    }
    return result;
  }

  async function cancelAcceptedTask(taskId) {
    if (taskId === undefined || taskId === null) return false;
    try {
      const response = await sendNativeWithRetry({
        type: 'cancel_task',
        task_id: Number(taskId),
        ...passiveStartupFields()
      });
      return Boolean(response && response.type === 'action_result' && response.ok);
    } catch (_error) {
      return false;
    }
  }

  async function cancelAcceptedOrKeepPending(pending, taskId) {
    const cancelled = await cancelAcceptedTask(taskId);
    pending.acceptedTaskId = taskId;
    pending.acceptedTaskCancelled = cancelled;
    return cancelled;
  }

  async function cancelPendingCurlTask(pending) {
    if (pending.acceptedTaskId === undefined || pending.acceptedTaskCancelled) return true;
    return cancelAcceptedTask(pending.acceptedTaskId);
  }

  async function openSettingsTab(downloadId) {
    const getUrl = browserApi.runtime.getURL
      ? browserApi.runtime.getURL('settings.html')
      : 'settings.html';
    const tab = await browserApi.tabs.create({
      url: `${getUrl}?downloadId=${encodeURIComponent(downloadId)}`
    });
    if (tab && tab.id !== undefined) settingsTabs.set(tab.id, downloadId);
    return tab;
  }

  async function openRetrySettingsTab(downloadId) {
    try {
      return await openSettingsTab(downloadId);
    } catch (_error) {
      await notifyFailure('Firefox 下載仍待處理；請從擴充功能重新開啟下載設定頁。');
      return null;
    }
  }

  async function handleCreatedDownload(download) {
    if (consumeManagedFallback(download)) return { ignored: true };
    if (!core.isSupportedDownloadUrl(download.url)) return { ignored: true };
    if (download.state && download.state !== 'in_progress') return { ignored: true };
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
      forceRecreate: true,
      firefoxDownloadRemoved: false,
      externalRequestId: null,
      acceptanceUnknown: false,
      operation: 'intercept'
    };
    pendingDownloads.set(download.id, pending);

    try {
      // Pause first so Firefox stops writing while the native item is being
      // cancelled; cancellation/erase immediately afterwards removes the
      // native download panel before the settings tab is shown.
      try { await browserApi.downloads.pause(download.id); } catch (_error) {
        pending.forceRecreate = false;
        const restored = await restoreAndReport(pending);
        if (!restored.ok) {
          restored.error = 'Firefox 下載未能恢復；請重新開啟下載設定頁後重試。';
          await notifyFailure(restored.error);
          await openRetrySettingsTab(download.id);
        }
        if (restored.ok) pendingDownloads.delete(download.id);
        pending.operation = null;
        return restored.ok ? { restored: true } : { restored: false, error: restored.error };
      }
      // Open the decision page before erasing the native item.  If tab
      // creation fails, the original download is still available to resume;
      // if erasing fails, the already-open page remains a retry path.
      const tab = await openSettingsTab(download.id);
      const removed = await cancelAndEraseDownload(pending);
      if (!removed && !pending.firefoxDownloadRemoved) {
        await notifyFailure('Firefox 原生下載視窗未能隱藏，請在設定頁重試。');
      }
      pending.operation = null;
      return { paused: true, tabId: tab && tab.id };
    } catch (_error) {
      // A settings tab failure happens before the native item is handed off;
      // restore the original item and never create an invisible fallback.
      pending.forceRecreate = false;
      const restored = await restoreAndReport(pending);
      if (!restored.ok) {
        restored.error = 'Firefox 下載未能恢復；請重新開啟下載設定頁後重試。';
        await notifyFailure(restored.error);
        await openRetrySettingsTab(download.id);
      }
      if (restored.ok) pendingDownloads.delete(download.id);
      pending.operation = null;
      return restored.ok ? { restored: true } : { restored: false, error: restored.error };
    }
  }

  async function submitExternalDownload(downloadId, form, startIntentUnixMs) {
    const pending = pendingDownloads.get(Number(downloadId));
    if (!pending) return { ok: false, error: '找不到暫停中的下載。' };
    if (pending.operation) return { ok: false, error: '此下載仍在處理中，請稍候再試。' };
    const validationError = validateForm(form);
    if (validationError) return { ok: false, error: validationError };
    pending.operation = 'submit';

    if (!pending.externalRequestId) {
      pending.externalRequestId = `enqueue-${pending.downloadId}-${now()}-${Math.random().toString(36).slice(2)}`;
    }
    const request = {
      ...core.buildEnqueueMessage(
        pending,
        form,
        pending.externalRequestId
      ),
      ...startupFields(true, startIntentUnixMs)
    };
    let accepted = false;
    try {
      const response = await sendNativeWithRetry(request);
      if (!response || response.type !== 'enqueue_result' || !response.ok) {
        if (response && response.type === 'enqueue_result'
            && response.error && response.error.code === 'engine_timeout') {
          // The engine may still persist the task after the IPC wait expires.
          // Keep the settings page and the request id so retrying reconciles
          // with the persisted task instead of restoring Firefox and creating
          // a duplicate download.
          await notifyFailure('Curl Downloader 仍在處理此任務；請保留設定頁並稍後重試。');
          pending.acceptanceUnknown = true;
          return {
            ok: false,
            code: 'enqueue_pending',
            error: 'Curl Downloader 仍在處理此任務；請保留設定頁並稍後重試。'
          };
        }
        throw new Error('Curl Downloader 未接收任務');
      }
      accepted = true;
      pending.acceptanceUnknown = false;
      if (!pending.firefoxDownloadRemoved && !(await cancelAndEraseDownload(pending))) {
        await notifyFailure('Curl Downloader 已接收任務，但 Firefox 原下載未能清理。');
        const taskCancelled = await cancelAcceptedOrKeepPending(pending, response.task_id);
        return {
          ok: false,
          code: 'firefox_cleanup_failed',
          taskId: response.task_id,
          taskCancelled,
          error: taskCancelled
            ? 'Curl Downloader 任務已取消，但 Firefox 原下載仍未能隱藏；請按「使用 Firefox」重試。'
            : 'Firefox 原下載仍未能隱藏，且 Curl Downloader 任務未能取消；請勿恢復 Firefox，稍後重試。'
        };
      }
      let awaitingFileDecision = response.awaiting_file_decision;
      if (awaitingFileDecision === undefined && response.task_id !== undefined) {
        try {
          const summary = await sendNativeWithRetry({
            type: 'list_tasks',
            ...passiveStartupFields()
          });
          const task = summary && Array.isArray(summary.tasks)
            ? summary.tasks.find((item) => Number(item.task_id) === Number(response.task_id))
            : null;
          awaitingFileDecision = task && (
            task.status === 'awaiting_file_decision' ||
            task.status === 'AwaitingFileDecision'
          );
        } catch (_error) {
          awaitingFileDecision = false;
        }
      }
      pendingDownloads.delete(pending.downloadId);
      restartDelayMs = 500;
      restartCooldownUntil = 0;
      if (!awaitingFileDecision && response.task_id !== undefined) {
        try {
          await sendNativeWithRetry({
            type: 'show_task',
            task_id: Number(response.task_id),
            ...passiveStartupFields()
          });
        } catch (_error) {
          // The task is already accepted; showing its page is best effort.
        }
      }
      void refreshTaskStatus();
      try {
        await storage.saveDefaults(form);
      } catch (_error) {
        // Saving defaults must never change the already accepted download.
      }
      return {
        ok: true,
        taskId: response.task_id,
        awaitingFileDecision: Boolean(awaitingFileDecision)
      };
    } catch (_error) {
      if (!accepted) {
        await notifyFailure('Native host 未能接收下載，已恢復 Firefox。');
        // Try resuming first for compatibility with a Firefox item that was
        // cancelled too late; restoreFirefoxDownload falls back to a fresh
        // Firefox item if resume is rejected after erase.
        pending.forceRecreate = false;
        const restored = await restoreAndReport(pending);
        if (restored.ok) pendingDownloads.delete(pending.downloadId);
        else await notifyFailure(restored.error);
      }
      return {
        ok: false,
        code: 'native_unavailable',
        error: 'Native host 未能接收下載；請保留此設定頁並稍後重試。'
      };
    } finally {
      if (pendingDownloads.get(pending.downloadId) === pending && pending.operation === 'submit') {
        pending.operation = null;
      }
    }
  }

  function validTaskId(value) {
    const taskId = Number(value);
    return Number.isSafeInteger(taskId) && taskId >= 0 ? taskId : null;
  }

  async function listTasks(options = {}) {
    const intent = Number(options.startIntentUnixMs);
    const startup = Number.isFinite(intent) && intent > 0
      ? startupFields(true, intent)
      : passiveStartupFields();
    try {
      const response = await sendNativeWithRetry({
        type: 'list_tasks',
        ...startup
      });
      if (!response || response.type !== 'task_list' || !Array.isArray(response.tasks)) {
        if (response && response.type === 'error') {
          const nativeError = response.error || {};
          return {
            ok: false,
            code: nativeError.code || 'native_unavailable',
            error: nativeError.message || 'Curl Downloader Native host 無法完成請求。'
          };
        }
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
      'open-folder': 'open_folder',
      'resolve-file-conflict': 'resolve_file_conflict'
    }[message.type];
    try {
      const request = { type: nativeType, task_id: taskId, ...passiveStartupFields() };
      if (message.type === 'resolve-file-conflict') request.decision = message.decision;
      const response = await sendNativeWithRetry(request);
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
      if (pending.operation) return { ok: false, error: '此下載仍在處理中，請稍候再試。' };
      if (pending.acceptanceUnknown) {
        return { ok: false, error: 'Curl Downloader 任務狀態尚未確認，請先按「提交」重試。' };
      }
      pending.operation = message.type === 'restore-firefox' ? 'restore' : 'fallback';
      try {
        if (pending.acceptedTaskId !== undefined) {
          const cancelled = await cancelPendingCurlTask(pending);
          if (!cancelled) {
            await openRetrySettingsTab(id);
            return { ok: false, error: 'Curl Downloader 任務尚未取消，為避免重複下載請稍後重試。' };
          }
        }
        const result = await restoreFirefoxDownload(pending);
        if (result.ok) pendingDownloads.delete(id);
        return result;
      } finally {
        if (pendingDownloads.get(id) === pending) pending.operation = null;
      }
    }
    if (message.type === 'cancel-download') {
      const pending = pendingDownloads.get(id);
      if (!pending) return { ok: false, error: '找不到下載項目。' };
      if (pending.operation) return { ok: false, error: '此下載仍在處理中，請稍候再試。' };
      if (pending.acceptanceUnknown) {
        return { ok: false, error: 'Curl Downloader 任務狀態尚未確認，請先按「提交」重試。' };
      }
      pending.operation = 'cancel';
      try {
        if (pending.acceptedTaskId !== undefined) {
          const cancelled = await cancelPendingCurlTask(pending);
          if (!cancelled) {
            await openRetrySettingsTab(id);
            return { ok: false, error: 'Curl Downloader 任務尚未取消，請稍後重試。' };
          }
        }
        const result = await cancelFirefoxDownload(pending);
        if (result.ok) pendingDownloads.delete(id);
        return result;
      } finally {
        if (pendingDownloads.get(id) === pending) pending.operation = null;
      }
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
    if (message.type === 'get-task-summary') {
      return refreshTaskStatus({ fromPopup: true, startIntentUnixMs: message.startIntentUnixMs });
    }
    if (message.type === 'popup-open') {
      popupOpen = true;
      updateNativeKeepAlive();
      return { ok: true };
    }
    if (message.type === 'popup-close') {
      popupOpen = false;
      updateNativeKeepAlive();
      return { ok: true };
    }
    if (message.type === 'show-task' || message.type === 'open-file' || message.type === 'open-folder') {
      return sendTaskAction(message);
    }
    if (message.type === 'resolve-file-conflict') return sendTaskAction(message);
    if (message.type === 'get-defaults') {
      try {
        const response = await sendNativeWithRetry({ type: 'get_defaults', ...startupFields(Boolean(message.autoStart), message.startIntentUnixMs) });
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
        void restorePendingAfterTabClose(pending).then((result) => {
          if (result.ok) pendingDownloads.delete(downloadId);
          else void notifyFailure('Firefox 下載未能恢復；請重新開啟設定頁後重試。');
        });
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
    validateForm,
    isBadgeSyncRunning: () => badgeSyncRunning
  };
});
