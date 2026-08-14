(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory(null);
  } else {
    root.CurlDownloaderPopup = factory(root.browser);
  }
})(typeof globalThis === 'object' ? globalThis : this, function (browserApi) {
  'use strict';

  const REFRESH_INTERVAL_MS = 200;
  const terminalStatuses = new Set(['completed', 'cancelled']);
  const statusLabels = {
    queued: '排隊中',
    probing: '檢查中',
    downloading: '下載中',
    pausing: '暫停中',
    paused: '已暫停',
    needs_proxy_password: '需要 Proxy 密碼',
    needs_firefox_authorization: '需要 Firefox 重新授權',
    awaiting_file_decision: '等待檔案決定',
    finalizing: '整理中',
    completed: '已完成',
    failed: '失敗',
    cancelled: '已取消'
  };
  const authorizationLabels = {
    public: '公開（無加密資料）',
    encrypted: 'Firefox 授權（DPAPI 加密）',
    needs_firefox_authorization: '需要 Firefox 重新授權',
    decryption_failed: '授權資料無法解密',
    protected_cleared: '受保護（授權資料已清除）'
  };

  function numberOrZero(value) {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : 0;
  }

  function formatBytes(value) {
    const bytes = numberOrZero(value);
    if (bytes < 1024) return `${Math.round(bytes)} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(2)} KiB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1_048_576).toFixed(2)} MiB`;
    return `${(bytes / 1_073_741_824).toFixed(2)} GiB`;
  }

  function formatProgress(downloaded, totalSize) {
    const current = numberOrZero(downloaded);
    const total = Number(totalSize);
    if (Number.isFinite(total) && total > 0) {
      return `${Math.min(100, current / total * 100).toFixed(1)}%`;
    }
    return current > 0 ? formatBytes(current) : '尚未開始';
  }

  function statusLabel(status) {
    return statusLabels[String(status)] || '未知狀態';
  }

  function splitTasks(tasks) {
    const active = [];
    const completed = [];
    for (const task of Array.isArray(tasks) ? tasks : []) {
      if (task && task.status === 'completed') {
        if (completed.length < 10) completed.push(task);
      } else if (task && !terminalStatuses.has(task.status)) {
        active.push(task);
      }
    }
    return { active, completed };
  }

  function popupRefreshRequest(startIntentUnixMs) {
    const request = { type: 'get-task-summary', autoStart: true };
    const intent = Number(startIntentUnixMs);
    if (Number.isFinite(intent) && intent > 0) request.startIntentUnixMs = intent;
    return request;
  }
  function formatDuration(seconds) {
    const value = Math.max(0, Math.round(numberOrZero(seconds)));
    if (value >= 3600) return `${Math.floor(value / 3600)}小時`;
    if (value >= 60) return `${Math.floor(value / 60)}分 ${value % 60}秒`;
    return `${value}秒`;
  }

  function formatSpeed(task) {
    const bps = task.status === 'downloading' || task.status === 'probing'
      ? task.current_bps
      : task.average_bps;
    return bps > 0 ? `${formatBytes(bps)}/s` : '速度 —';
  }

  function textElement(documentApi, tag, className, text) {
    const element = documentApi.createElement(tag);
    element.className = className;
    element.textContent = text;
    return element;
  }

  function renderTask(documentApi, task, sendAction, showError) {
    const card = documentApi.createElement('article');
    card.className = 'task-card';
    card.tabIndex = 0;
    card.setAttribute('role', 'button');
    card.setAttribute('aria-label', `在 Curl Downloader 顯示 ${task.filename || '下載任務'}`);

    const main = documentApi.createElement('div');
    main.className = 'task-main';
    const filename = String(task.filename || '未命名下載');
    const filenameElement = textElement(documentApi, 'strong', 'filename', filename);
    filenameElement.setAttribute('title', filename);
    main.append(
      filenameElement,
      textElement(documentApi, 'span', 'status', statusLabel(task.status))
    );
    const authorizationLabel = authorizationLabels[String(task.authorization)];
    if (authorizationLabel) {
      main.append(textElement(documentApi, 'span', 'authorization', authorizationLabel));
    }
    card.append(main);

    const total = Number(task.total_size);
    const fraction = Number.isFinite(total) && total > 0
      ? Math.min(1, numberOrZero(task.downloaded) / total)
      : task.status === 'completed' ? 1 : 0;
    const track = documentApi.createElement('div');
    track.className = 'progress-track';
    const fill = documentApi.createElement('div');
    fill.className = 'progress-fill';
    fill.style.width = `${fraction * 100}%`;
    track.append(fill);
    card.append(track);

    const meta = documentApi.createElement('div');
    meta.className = 'task-meta';
    const progress = Number.isFinite(total) && total > 0
      ? `${formatProgress(task.downloaded, total)} · ${formatBytes(task.downloaded)} / ${formatBytes(total)}`
      : formatProgress(task.downloaded, total);
    meta.append(
      textElement(documentApi, 'span', '', progress),
      textElement(documentApi, 'span', '', formatSpeed(task))
    );
    if (task.eta_seconds !== null && task.eta_seconds !== undefined) {
      meta.append(textElement(documentApi, 'span', '', `ETA ${formatDuration(task.eta_seconds)}`));
    }
    card.append(meta);
    card.append(textElement(documentApi, 'div', 'target', task.target_dir || '目標資料夾未設定'));

    const actions = documentApi.createElement('div');
    actions.className = 'task-actions';
    const bindOpenAction = (button, type, decision) => {
      let inFlight = false;
      button.addEventListener('click', (event) => {
        event.stopPropagation();
        if (inFlight) return;
        inFlight = true;
        button.disabled = true;
        Promise.resolve(sendAction(type, task.task_id, decision))
          .catch(showError)
          .finally(() => {
            inFlight = false;
            button.disabled = false;
          });
      });
    };
    if (task.file_available) {
      const button = documentApi.createElement('button');
      button.type = 'button';
      button.textContent = '開啟檔案';
      bindOpenAction(button, 'open-file');
      actions.append(button);
    }
    if (task.folder_available) {
      const button = documentApi.createElement('button');
      button.type = 'button';
      button.textContent = '開啟資料夾';
      bindOpenAction(button, 'open-folder');
      actions.append(button);
    }
    if (task.status === 'awaiting_file_decision' && task.origin === 'firefox') {
      const overwrite = documentApi.createElement('button');
      overwrite.type = 'button';
      overwrite.textContent = '覆蓋';
      bindOpenAction(overwrite, 'resolve-file-conflict', 'overwrite');
      actions.append(overwrite);
      const cancel = documentApi.createElement('button');
      cancel.type = 'button';
      cancel.className = 'secondary';
      cancel.textContent = '取消任務';
      bindOpenAction(cancel, 'resolve-file-conflict', 'cancel');
      actions.append(cancel);
    }
    if (task.status === 'needs_firefox_authorization' && task.origin === 'firefox') {
      const reauthorize = documentApi.createElement('button');
      reauthorize.type = 'button';
      reauthorize.textContent = '在 Firefox 重新授權';
      bindOpenAction(reauthorize, 'reauthorize-firefox');
      actions.append(reauthorize);
    }
    if (actions.childElementCount > 0) card.append(actions);

    const showTask = () => { void sendAction('show-task', task.task_id).catch(showError); };
    card.addEventListener('click', showTask);
    card.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        showTask();
      }
    });
    return card;
  }

  function renderTaskGroup(documentApi, container, tasks, emptyText, sendAction, showError) {
    container.replaceChildren();
    if (!tasks.length) {
      container.append(textElement(documentApi, 'p', 'empty', emptyText));
      return;
    }
    const fragment = documentApi.createDocumentFragment();
    for (const task of tasks) fragment.append(renderTask(documentApi, task, sendAction, showError));
    container.append(fragment);
  }

  function startPopup(documentApi = typeof document === 'undefined' ? null : document, api = browserApi) {
    if (!documentApi || !api || !api.runtime || !api.runtime.sendMessage) return null;
    const activeContainer = documentApi.getElementById('active-tasks');
    const completedContainer = documentApi.getElementById('completed-tasks');
    const status = documentApi.getElementById('connection-status');
    const error = documentApi.getElementById('error');
    const activeCount = documentApi.getElementById('active-count');
    const completedCount = documentApi.getElementById('completed-count');
    let refreshInFlight = false;
    let timer = null;
    let stopped = false;

    function notifyLifecycle(type) {
      try {
        const result = api.runtime.sendMessage({ type });
        if (result && typeof result.catch === 'function') result.catch(() => undefined);
      } catch (_error) {
        // Popup teardown must not surface a rejected lifecycle notification.
      }
    }

    notifyLifecycle('popup-open');
    let popupStartIntentUnixMs = Date.now();

    function showError(message) {
      error.textContent = message || 'Curl Downloader 操作失敗。';
      error.hidden = false;
    }

    async function sendAction(type, taskId, decision) {
      const message = { type, taskId };
      if (decision) message.decision = decision;
      const response = await api.runtime.sendMessage(message);
      if (!response || !response.ok) throw new Error(response && response.error || 'Curl Downloader 操作失敗。');
      return response;
    }

    async function refresh() {
      if (refreshInFlight) return;
      refreshInFlight = true;
      try {
        const request = popupRefreshRequest(popupStartIntentUnixMs);
        popupStartIntentUnixMs = undefined;
        const response = await api.runtime.sendMessage(request);
        if (!response || !response.ok) throw new Error(response && response.error || '未能讀取任務。');
        const groups = splitTasks(response.tasks);
        status.textContent = '已連線';
        status.className = 'connection ready';
        error.hidden = true;
        activeCount.textContent = String(groups.active.length);
        completedCount.textContent = String(groups.completed.length);
        renderTaskGroup(documentApi, activeContainer, groups.active, '目前沒有進行中的任務。', sendAction, showError);
        renderTaskGroup(documentApi, completedContainer, groups.completed, '目前沒有最近完成的任務。', sendAction, showError);
      } catch (reason) {
        status.textContent = '未連線';
        status.className = 'connection offline';
        showError(reason && reason.message ? reason.message : String(reason));
      } finally {
        refreshInFlight = false;
      }
    }

    void refresh();
    timer = setInterval(() => { void refresh(); }, REFRESH_INTERVAL_MS);
    const stop = () => {
      if (timer !== null) clearInterval(timer);
      if (stopped) return;
      stopped = true;
      timer = null;
      notifyLifecycle('popup-close');
    };
    if (typeof window !== 'undefined' && window.addEventListener) {
      window.addEventListener('pagehide', stop, { once: true });
    }
    return { refresh, stop };
      window.addEventListener('unload', stop, { once: true });
  }

  if (typeof document !== 'undefined') {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', () => { startPopup(); }, { once: true });
    } else {
      startPopup();
    }
  }

  return { formatBytes, formatProgress, statusLabel, splitTasks, popupRefreshRequest, renderTask, startPopup, REFRESH_INTERVAL_MS };
});
