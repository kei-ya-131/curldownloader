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
    const now = typeof options.now === 'function' ? options.now : Date.now;
    let port = null;
    let idleTimer = null;
    let keepAlive = false;
    let requestSequence = 0;
    const pending = new Map();

    function clearIdleTimer() {
      if (idleTimer !== null) clearTimeout(idleTimer);
      idleTimer = null;
    }

    function rejectPending(error) {
      for (const request of pending.values()) request.reject(error);
      pending.clear();
    }

    function handleMessage(response) {
      const requestId = response && response.request_id;
      if (!requestId || !pending.has(requestId)) return;
      const request = pending.get(requestId);
      pending.delete(requestId);
      request.resolve(response);
      scheduleIdleClose();
    }

    function handleDisconnect() {
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
      port.onMessage.addListener(handleMessage);
      port.onDisconnect.addListener(handleDisconnect);
      return port;
    }

    function scheduleIdleClose() {
      clearIdleTimer();
      if (keepAlive || pending.size > 0 || !port) return;
      idleTimer = setTimeout(() => {
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
        let activePort;
        try {
          activePort = ensurePort();
        } catch (error) {
          reject(error);
          return;
        }
        clearIdleTimer();
        pending.set(requestId, { resolve, reject });
        try {
          activePort.postMessage(request);
        } catch (error) {
          pending.delete(requestId);
          reject(error);
          handleDisconnect();
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
