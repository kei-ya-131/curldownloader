const test = require('node:test');
const assert = require('node:assert/strict');
const status = require('../status.js');
const { summarizeTasks, badgeState } = status;

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

test('selects the nearest Cyber progress icon while preserving base sizes', () => {
  assert.deepEqual(status.iconDetails(48), { path: 'icons/progress-050.png' });
  assert.deepEqual(status.iconDetails(null), {
    path: {
      16: 'icons/curl-downloader-16.png',
      32: 'icons/curl-downloader-32.png',
      48: 'icons/curl-downloader-48.png'
    }
  });
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