const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const storage = require('../storage.js');

test('segment defaults use four and preserve valid boundary values', () => {
  assert.equal(storage.cleanDefaults({}).segments, 4);
  assert.equal(storage.cleanDefaults({ segments: 1 }).segments, 1);
  assert.equal(storage.cleanDefaults({ segments: 8 }).segments, 8);
});

test('invalid stored segment values recover to four', () => {
  for (const value of [0, 9, 2.5, 'four', null]) {
    assert.equal(storage.cleanDefaults({ segments: value }).segments, 4);
  }
});

test('settings page exposes a bounded segment control', () => {
  const html = fs.readFileSync(path.join(__dirname, '..', 'settings.html'), 'utf8');
  assert.match(html, /id="segments"/);
  assert.match(html, /min="1"/);
  assert.match(html, /max="8"/);
  assert.match(html, /下載線程數量（分段）/);
});