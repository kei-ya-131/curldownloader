const test = require('node:test');
const assert = require('node:assert/strict');
const { summarizeTasks, badgeState } = require('../status.js');

test('weights overall progress by total bytes and counts active tasks', () => {
  const summary = summarizeTasks([
    { status: 'downloading', downloaded: 50, total_size: 100 },
    { status: 'downloading', downloaded: 900, total_size: 1900 },
    { status: 'completed', downloaded: 100, total_size: 100 }
  ]);
  assert.equal(summary.activeCount, 2);
  assert.equal(summary.percent, 48);
  assert.equal(badgeState(summary).text, '48%/2');
});

test('uses an indeterminate percentage when all active totals are unknown', () => {
  const summary = summarizeTasks([{ status: 'downloading', downloaded: 20, total_size: null }]);
  assert.equal(summary.percent, null);
  assert.equal(badgeState(summary).text, '—/1');
});

test('failed and cancelled tasks do not count as active but failures produce warning state', () => {
  const summary = summarizeTasks([
    { status: 'failed', downloaded: 10, total_size: 100 },
    { status: 'cancelled', downloaded: 10, total_size: 100 }
  ]);
  assert.equal(summary.activeCount, 0);
  assert.equal(summary.hasFailure, true);
  assert.equal(badgeState(summary).text, '');
});