const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { formatBytes, formatProgress, statusLabel, splitTasks, popupRefreshRequest, renderTask, REFRESH_INTERVAL_MS } = require('../popup.js');
test('popup sends one explicit start intent and later passive refreshes', () => {
  assert.deepEqual(popupRefreshRequest(1234), {
    type: 'get-task-summary',
    autoStart: true,
    startIntentUnixMs: 1234
  });
  assert.deepEqual(popupRefreshRequest(), { type: 'get-task-summary', autoStart: true });
});
test('refreshes task summaries frequently enough for fast downloads', () => {
  assert.ok(REFRESH_INTERVAL_MS <= 250);
});

test('long task filenames cannot expand the popup card', () => {
  const css = fs.readFileSync(path.join(__dirname, '..', 'popup.css'), 'utf8');
  assert.match(css, /\.task-list\s*\{[^}]*min-width:\s*0/s);
  assert.match(css, /\.task-card\s*\{[^}]*min-width:\s*0/s);
  assert.match(css, /\.task-main\s*\{[^}]*min-width:\s*0/s);
  assert.match(css, /\.filename\s*\{[^}]*flex:\s*1 1 auto/s);
  assert.match(css, /\.filename\s*\{[^}]*text-overflow:\s*ellipsis/s);
});

test('formats task summary values for the popup', () => {
  assert.equal(formatBytes(1_048_576), '1.00 MiB');
  assert.equal(formatProgress(512, 1024), '50.0%');
  assert.equal(formatProgress(512, null), '512 B');
  assert.equal(statusLabel('downloading'), '下載中');
  assert.equal(statusLabel('completed'), '已完成');
});

test('splitTasks keeps all ongoing tasks and only ten recent completed tasks', () => {
  const tasks = [
    { task_id: 1, status: 'queued' },
    ...Array.from({ length: 11 }, (_value, index) => ({
      task_id: 100 + index,
      status: 'completed'
    })),
    { task_id: 2, status: 'failed' },
    { task_id: 3, status: 'cancelled' }
  ];
  const groups = splitTasks(tasks);
  assert.deepEqual(groups.active.map((task) => task.task_id), [1, 2]);
  assert.deepEqual(groups.completed.map((task) => task.task_id), [100, 101, 102, 103, 104, 105, 106, 107, 108, 109]);
});
function fakeElement() {
  return {
    children: [],
    childElementCount: 0,
    className: '',
    disabled: false,
    style: {},
    append(...items) {
      this.children.push(...items);
      this.childElementCount = this.children.length;
    },
    addEventListener(type, listener) {
      this.listeners = this.listeners || {};
      this.listeners[type] = listener;
    },
    click() {
      return this.listeners && this.listeners.click
        ? this.listeners.click({ stopPropagation() {} })
        : undefined;
    },
    setAttribute(name, value) {
      this.attributes = this.attributes || {};
      this.attributes[name] = value;
    }
  };
}

test('ellipsized filename keeps the complete name as a tooltip', () => {
  const documentApi = { createElement: () => fakeElement() };
  const filename = 'Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-IQ3_M.gguf';
  const card = renderTask(documentApi, {
    task_id: 9,
    filename,
    status: 'downloading',
    downloaded: 1,
    total_size: 2,
    target_dir: 'D:\\\\llama\\\\models',
    file_available: false,
    folder_available: true
  }, async () => ({ ok: true }), () => {});
  const filenameElement = card.children[0].children[0];
  assert.equal(filenameElement.attributes.title, filename);
});

test('open action button ignores duplicate clicks while its request is pending', async () => {
  const documentApi = { createElement: () => fakeElement() };
  let resolveAction;
  const actions = [];
  const card = renderTask(documentApi, {
    task_id: 7,
    filename: 'file.zip',
    status: 'completed',
    downloaded: 10,
    total_size: 10,
    target_dir: 'C:\\Downloads',
    file_available: true,
    folder_available: false
  }, (type, taskId) => {
    actions.push({ type, taskId });
    return new Promise((resolve) => { resolveAction = resolve; });
  }, () => {});
  const button = card.children.at(-1).children[0];
  button.click();
  button.click();
  assert.deepEqual(actions, [{ type: 'open-file', taskId: 7 }]);
  assert.equal(button.disabled, true);
  resolveAction({ ok: true });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(button.disabled, false);
});