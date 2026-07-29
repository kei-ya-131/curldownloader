const test = require('node:test');
const assert = require('node:assert/strict');
const { formatBytes, formatProgress, statusLabel, splitTasks, popupRefreshRequest, REFRESH_INTERVAL_MS } = require('../popup.js');
test('opening and refreshing the popup never starts a manually stopped exe', () => {
  assert.deepEqual(popupRefreshRequest(), {
    type: 'get-task-summary',
    autoStart: false
  });
});
test('refreshes task summaries frequently enough for fast downloads', () => {
  assert.ok(REFRESH_INTERVAL_MS <= 250);
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