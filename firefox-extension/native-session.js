(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory();
  } else {
    root.CurlDownloaderNativeSession = factory();
  }
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';

  function createNativeSession(browserApi, options = {}) {
    const runtime = browserApi && browserApi.runtime;
    const idleMs = Number.isFinite(options.idleMs)
      ? Math.max(0, options.idleMs)
      : 1500;
    const responseTimeoutMs = Number.isFinite(options.responseTimeoutMs)
      ? Math.max(1, options.responseTimeoutMs)
      : 20_000;
    const pickFolderTimeoutMs = Number.isFinite(options.pickFolderTimeoutMs)
      ? Math.max(125_000, options.pickFolderTimeoutMs)
      : 130_000;
    const now = typeof options.now === 'function' ? options.now : Date.now;
    const setTimer = typeof options.setTimeout === 'function' ? options.setTimeout : setTimeout;
    const clearTimer = typeof options.clearTimeout === 'function' ? options.clearTimeout : clearTimeout;
    let port = null;
    let idleTimer = null;
    let keepAlive = false;
    let requestSequence = 0;
    const pending = new Map();

    function clearIdleTimer() {
      if (idleTimer !== null) clearTimer(idleTimer);
      idleTimer = null;
    }

    function rejectPending(error) {
      for (const request of pending.values()) {
        clearTimer(request.timeout);
        request.reject(error);
      }
      pending.clear();
    }

    function handleMessage(response, sourcePort) {
      if (sourcePort !== port) return;
      const requestId = response && response.request_id;
      if (!requestId || !pending.has(requestId)) return;
      const request = pending.get(requestId);
      pending.delete(requestId);
      clearTimer(request.timeout);
      request.resolve(response);
      scheduleIdleClose();
    }

    function handleDisconnect(sourcePort) {
      if (sourcePort !== port) return;
      const error = new Error('Native host disconnected');
      const disconnected = port;
      port = null;
      clearIdleTimer();
      rejectPending(error);
      if (disconnected && typeof disconnected.onDisconnect === 'object') {
        // Firefox owns the lastError object; no response is required here.
      }
    }

    function ensurePort() {
      if (port) return port;
      if (!runtime || typeof runtime.connectNative !== 'function') {
        throw new Error('Firefox 不支援持續 Native Messaging');
      }
      port = runtime.connectNative('curl_downloader');
      const activePort = port;
      port.onMessage.addListener((response) => handleMessage(response, activePort));
      port.onDisconnect.addListener(() => handleDisconnect(activePort));
      return port;
    }

    function scheduleIdleClose() {
      clearIdleTimer();
      if (keepAlive || pending.size > 0 || !port) return;
      idleTimer = setTimer(() => {
        idleTimer = null;
        if (!keepAlive && pending.size === 0 && port) close('Native session idle');
      }, idleMs);
      if (typeof idleTimer.unref === 'function') idleTimer.unref();
    }

    function send(message) {
      const requestId = message && message.request_id
        ? String(message.request_id)
        : `firefox-${now()}-${requestSequence++}`;
      const request = { ...(message || {}), request_id: requestId };
      return new Promise((resolve, reject) => {
        if (pending.has(requestId)) {
          reject(new Error(`Duplicate native request id: ${requestId}`));
          return;
        }
        let activePort;
        try {
          activePort = ensurePort();
        } catch (error) {
          reject(error);
          return;
        }
        clearIdleTimer();
        const latestPickerDeadline = Math.max(0, ...[...pending.values()]
            .filter((item) => item.type === 'pick_folder')
            .map((item) => item.folderDeadlineUnixMs));
        let timeoutMs = request.type === 'pick_folder'
          ? Math.max(pickFolderTimeoutMs, latestPickerDeadline - now() + pickFolderTimeoutMs)
          : Math.max(responseTimeoutMs, latestPickerDeadline - now() + responseTimeoutMs);
        if (!Number.isFinite(timeoutMs)) {
          timeoutMs = request.type === 'pick_folder' ? pickFolderTimeoutMs : responseTimeoutMs;
        }
        const timeout = setTimer(() => {
          if (!pending.has(requestId)) return;
          pending.delete(requestId);
          reject(new Error(`Native host response timed out after ${timeoutMs} ms`));
          // Closing the timed-out port keeps a late response with this id
          // from being confused with a retry using the same id.
          close('Native response timed out');
        }, timeoutMs);
        pending.set(requestId, {
          resolve,
          reject,
          timeout,
          type: request.type,
          folderDeadlineUnixMs: request.type === 'pick_folder' ? now() + timeoutMs : null
        });
        try {
          activePort.postMessage(request);
        } catch (error) {
          const item = pending.get(requestId);
          if (item) clearTimer(item.timeout);
          pending.delete(requestId);
          reject(error);
          handleDisconnect(activePort);
        }
      });
    }

    function setKeepAlive(value) {
      keepAlive = Boolean(value);
      if (keepAlive) clearIdleTimer();
      else scheduleIdleClose();
    }

    function close(reason) {
      clearIdleTimer();
      const closingPort = port;
      port = null;
      rejectPending(new Error(reason || 'Native session closed'));
      if (closingPort && typeof closingPort.disconnect === 'function') {
        try { closingPort.disconnect(); } catch (_error) { /* already disconnected */ }
      }
    }

    return {
      send,
      setKeepAlive,
      close,
      isConnected: () => port !== null
    };
  }

  return createNativeSession;
});
