(function () {
  'use strict';

  const params = new URLSearchParams(window.location.search);
  const downloadId = Number(params.get('downloadId'));
  const formElement = document.getElementById('download-form');
  const statusElement = document.getElementById('status');
  const proxyFields = document.getElementById('proxy-fields');
  const submitButton = document.getElementById('submit-external');
  let pending;

  function setStatus(message, kind) {
    statusElement.textContent = message;
    statusElement.className = `status ${kind || ''}`.trim();
  }

  function setProxyEnabled(enabled) {
    proxyFields.setAttribute('aria-disabled', String(!enabled));
    for (const field of proxyFields.querySelectorAll('input, select')) {
      field.disabled = !enabled;
    }
  }

  function fillForm(download, defaults) {
    document.getElementById('url').value = download.url || '';
    document.getElementById('filename').value = download.filename || 'download.bin';
    document.getElementById('target-dir').value = download.targetDir || defaults.targetDir || '';
    const proxy = download.proxy || defaults.proxy;
    document.getElementById('proxy-enabled').checked = Boolean(proxy.enabled);
    document.getElementById('proxy-protocol').value = proxy.protocol || 'http';
    document.getElementById('proxy-host').value = proxy.host || '';
    document.getElementById('proxy-port').value = proxy.port || 8080;
    document.getElementById('proxy-username').value = proxy.username || '';
    document.getElementById('proxy-password').value = '';
    setProxyEnabled(Boolean(proxy.enabled));
  }

  function readForm() {
    return {
      filename: document.getElementById('filename').value.trim(),
      targetDir: document.getElementById('target-dir').value.trim(),
      proxy: {
        enabled: document.getElementById('proxy-enabled').checked,
        protocol: document.getElementById('proxy-protocol').value,
        host: document.getElementById('proxy-host').value.trim(),
        port: document.getElementById('proxy-port').value,
        username: document.getElementById('proxy-username').value,
        password: document.getElementById('proxy-password').value
      }
    };
  }

  async function closeSettingsPage() {
    try {
      if (browser.tabs && browser.tabs.getCurrent && browser.tabs.remove) {
        const tab = await browser.tabs.getCurrent();
        if (tab && tab.id !== undefined) {
          await browser.tabs.remove(tab.id);
          return;
        }
      }
    } catch (_error) {
      // Fall back to closing the extension page below.
    }
    window.close();
  }

  async function restoreFirefox() {
    await browser.runtime.sendMessage({ type: 'restore-firefox', downloadId });
    await closeSettingsPage();
  }

  async function submitExternal(event) {
    event.preventDefault();
    const form = readForm();
    if (!form.filename || !form.targetDir) {
      setStatus('請填寫下載名稱及絕對下載目錄。', 'error');
      return;
    }
    submitButton.disabled = true;
    setStatus('正在交給 Curl Downloader…');
    try {
      const response = await browser.runtime.sendMessage({ type: 'submit-external', downloadId, form });
      if (!response || !response.ok) {
        const message = response && response.error ? response.error : 'Native host 未能接收任務';
        if (!message.startsWith('Native host')) {
          throw new Error(message);
        }
        setStatus(`${message} 正在關閉設定頁…`, 'error');
        setTimeout(() => { void closeSettingsPage(); }, 700);
        return;
      }
      await CurlExtensionStorage.saveDefaults(form);
      setStatus('已交給 Curl Downloader。', 'success');
      await closeSettingsPage();
    } catch (error) {
      submitButton.disabled = false;
      setStatus(error.message || '提交失敗，已恢復 Firefox 下載。', 'error');
    }
  }

  document.getElementById('proxy-enabled').addEventListener('change', (event) => {
    setProxyEnabled(event.target.checked);
  });
  document.getElementById('browse').addEventListener('click', async () => {
    const response = await browser.runtime.sendMessage({ type: 'pick-folder', downloadId });
    if (response && response.ok && response.targetDir) {
      document.getElementById('target-dir').value = response.targetDir;
    } else if (response && response.error) {
      setStatus(response.error, 'error');
    }
  });
  formElement.addEventListener('submit', submitExternal);
  document.getElementById('use-firefox').addEventListener('click', restoreFirefox);
  document.getElementById('cancel').addEventListener('click', restoreFirefox);

  (async () => {
    if (!Number.isInteger(downloadId)) {
      setStatus('找不到下載項目。', 'error');
      return;
    }
    try {
      pending = await browser.runtime.sendMessage({ type: 'get-pending', downloadId });
      if (!pending || !pending.ok) {
        throw new Error('找不到下載項目。');
      }
      const defaults = await CurlExtensionStorage.loadDefaults();
      let nativeDefaults = { targetDir: '' };
      try {
        nativeDefaults = await browser.runtime.sendMessage({ type: 'get-defaults' });
      } catch (_error) {
        // The form remains usable with the saved extension default.
      }
      fillForm({
        ...pending.download,
        targetDir: pending.download.targetDir || (nativeDefaults && nativeDefaults.targetDir) || ''
      }, defaults);
      if (nativeDefaults && nativeDefaults.ok === false && nativeDefaults.error) {
        setStatus(`${nativeDefaults.error}；請先註冊 Native host。`, 'error');
      } else {
        setStatus('原 Firefox 下載目前保持暫停，請選擇處理方式。');
      }
    } catch (error) {
      setStatus(error.message || '讀取下載資料失敗。', 'error');
      formElement.querySelectorAll('input, select, button').forEach((field) => { field.disabled = true; });
    }
  })();
})();
