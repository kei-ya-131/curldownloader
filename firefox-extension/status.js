(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory();
  } else {
    root.CurlExtensionStatus = factory();
  }
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';

  const ACTIVE_STATUSES = new Set([
    'queued',
    'probing',
    'downloading',
    'pausing',
    'paused',
    'needs_proxy_password',
    'awaiting_file_decision',
    'finalizing'
  ]);

  function nonNegativeNumber(value) {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : 0;
  }

  function summarizeTasks(tasks) {
    let activeCount = 0;
    let knownBytes = 0;
    let downloadedBytes = 0;
    let hasFailure = false;
    let hasProxyPassword = false;

    for (const task of Array.isArray(tasks) ? tasks : []) {
      const status = task && String(task.status || '');
      if (status === 'failed') hasFailure = true;
      if (status === 'needs_proxy_password') hasProxyPassword = true;
      if (!ACTIVE_STATUSES.has(status)) continue;

      activeCount += 1;
      const total = Number(task.total_size);
      if (Number.isFinite(total) && total > 0) {
        knownBytes += total;
        downloadedBytes += Math.min(nonNegativeNumber(task.downloaded), total);
      }
    }

    const percent = knownBytes > 0
      ? Math.min(100, Math.max(0, Math.round(downloadedBytes / knownBytes * 100)))
      : null;
    return {
      activeCount,
      knownBytes,
      downloadedBytes,
      percent,
      hasFailure,
      hasProxyPassword,
      hasActive: activeCount > 0
    };
  }

  function badgeState(summary) {
    const activeCount = Number(summary && summary.activeCount) || 0;
    const hasFailure = Boolean(summary && summary.hasFailure);
    const hasProxyPassword = Boolean(summary && summary.hasProxyPassword);
    if (activeCount <= 0) {
      return {
        text: '',
        color: hasFailure || hasProxyPassword ? '#d946ef' : '#0f172a',
        title: hasFailure
          ? 'Curl Downloader｜有失敗任務'
          : hasProxyPassword
            ? 'Curl Downloader｜需要 Proxy 密碼'
            : 'Curl Downloader',
        progressStep: null
      };
    }

    const percent = summary && Number.isInteger(summary.percent) ? summary.percent : null;
    const percentText = percent === null ? '—' : `${percent}%`;
    const warning = hasFailure || hasProxyPassword;
    const warningText = hasFailure
      ? '，有失敗任務'
      : hasProxyPassword
        ? '，需要 Proxy 密碼'
        : '';
    return {
      text: `${percentText}/${activeCount}`,
      color: warning ? '#d946ef' : '#00d9ff',
      title: `Curl Downloader｜整體 ${percentText}，${activeCount} 個進行中${warningText}`,
      progressStep: percent === null ? null : Math.min(100, Math.max(0, Math.round(percent / 10) * 10))
    };
  }

  const BASE_ICON_PATHS = {
    16: 'icons/curl-downloader-16.png',
    32: 'icons/curl-downloader-32.png',
    48: 'icons/curl-downloader-48.png'
  };

  function iconDetails(progressStep) {
    if (!Number.isInteger(progressStep)) return { path: BASE_ICON_PATHS };
    const percent = Math.min(100, Math.max(0, Math.round(progressStep / 10) * 10));
    return { path: `icons/progress-${String(percent).padStart(3, '0')}.png` };
  }

  return { ACTIVE_STATUSES, summarizeTasks, badgeState, iconDetails, BASE_ICON_PATHS };
});
