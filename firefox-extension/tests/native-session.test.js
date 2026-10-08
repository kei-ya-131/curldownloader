const test = require('node:test');
const assert = require('node:assert/strict');
const createNativeSession = require('../native-session.js');

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function makeFakeNativePort() {
  const messages = [];
  const state = {
    connectCalls: 0,
    disconnectCalls: 0,
    onMessage: null,
    onDisconnect: null
  };
  const browser = {
    runtime: {
      connectNative() {
        state.connectCalls += 1;
        return {
          onMessage: {
            addListener(listener) { state.onMessage = listener; }
          },
          onDisconnect: {
            addListener(listener) { state.onDisconnect = listener; }
          },
          postMessage(message) { messages.push(message); },
          disconnect() { state.disconnectCalls += 1; }
        };
      }
    }
  };
  return {
    browser,
    state,
    get messages() { return messages; },
    replyToAll() {
      const pending = messages.splice(0);
      for (const message of pending) {
        state.onMessage({
          type: 'task_list',
          request_id: message.request_id,
          tasks: []
        });
      }
    },
    respond(requestId, type) {
      state.onMessage({ type, request_id: requestId, ok: true });
    },
    disconnect() {
      state.onDisconnect();
    }
  };
}

function makeFakeTimers(startMs = 0) {
  let currentMs = startMs;
  let nextId = 1;
  const timers = new Map();
  return {
    now: () => currentMs,
    timers,
    setTimeout(callback, delayMs) {
      const timer = { id: nextId++ };
      timers.set(timer.id, { callback, delayMs, deadlineMs: currentMs + delayMs });
      return timer;
    },
    clearTimeout(timer) {
      if (timer) timers.delete(timer.id);
    },
    advance(milliseconds) {
      currentMs += milliseconds;
      const due = [...timers.entries()]
        .filter(([, timer]) => timer.deadlineMs <= currentMs)
        .sort((left, right) => left[1].deadlineMs - right[1].deadlineMs);
      for (const [id, timer] of due) {
        if (!timers.delete(id)) continue;
        timer.callback();
      }
    }
  };
}

test('reuses one native port for multiple requests', async () => {
  const fake = makeFakeNativePort();
  const session = createNativeSession(fake.browser, { idleMs: 20 });
  const first = session.send({ type: 'list_tasks' });
  const second = session.send({ type: 'get_defaults' });
  fake.replyToAll();
  await Promise.all([first, second]);
  assert.equal(fake.state.connectCalls, 1);
});

test('disconnect rejects every pending request and clears the port', async () => {
  const fake = makeFakeNativePort();
  const session = createNativeSession(fake.browser);
  const pending = session.send({ type: 'list_tasks' });
  fake.disconnect();
  await assert.rejects(pending, /disconnected/);
  assert.equal(session.isConnected(), false);
});

test('a duplicate request id is rejected without replacing the original pending request', async () => {
  const fake = makeFakeNativePort();
  const session = createNativeSession(fake.browser, { idleMs: 20 });
  const first = session.send({ type: 'list_tasks', request_id: 'same-id' });
  const duplicate = session.send({ type: 'get_defaults', request_id: 'same-id' });

  await assert.rejects(duplicate, /duplicate.*request id/i);
  assert.equal(fake.messages.length, 1);
  fake.replyToAll();
  const response = await first;
  assert.equal(response.request_id, 'same-id');
});

test('a native response timeout rejects the request and ignores a late response', async () => {
  const fake = makeFakeNativePort();
  const session = createNativeSession(fake.browser, { responseTimeoutMs: 5 });
  const pending = session.send({ type: 'list_tasks', request_id: 'slow' });

  await assert.rejects(pending, /timed out/i);
  fake.state.onMessage({ type: 'task_list', request_id: 'slow', tasks: [] });
  assert.equal(session.isConnected(), false);
});

test('normal requests wait through a pending folder picker plus their response budget', async () => {
  const fake = makeFakeNativePort();
  const timers = makeFakeTimers();
  const session = createNativeSession(fake.browser, {
    idleMs: 20,
    now: timers.now,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout
  });
  const picker = session.send({ type: 'pick_folder', request_id: 'picker' });
  const pickerOutcome = picker.then((value) => ({ value }), (error) => ({ error }));
  timers.advance(10_000);
  const normal = session.send({ type: 'list_tasks', request_id: 'queued-normal' });
  const normalOutcome = normal.then((value) => ({ value }), (error) => ({ error }));

  try {
    const normalTimer = [...timers.timers.values()].find((timer) => timer.deadlineMs > 130_000);
    assert.ok(normalTimer, 'normal request should receive a timer after the picker deadline');
    assert.equal(normalTimer.delayMs, 140_000);
    fake.respond('picker', 'folder_result');
    fake.respond('queued-normal', 'task_list');
    const [pickerResult, normalResult] = await Promise.all([pickerOutcome, normalOutcome]);
    assert.equal(pickerResult.value.type, 'folder_result');
    assert.equal(normalResult.value.type, 'task_list');
    session.close();
    assert.equal(timers.timers.size, 0);
  } finally {
    session.close();
    await Promise.all([pickerOutcome, normalOutcome]);
  }
});

test('queued folder pickers extend only the picker queue deadline', async () => {
  const fake = makeFakeNativePort();
  const timers = makeFakeTimers();
  const session = createNativeSession(fake.browser, {
    now: timers.now,
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout
  });
  const firstPicker = session.send({ type: 'pick_folder', request_id: 'picker-one' });
  const secondPicker = session.send({ type: 'pick_folder', request_id: 'picker-two' });
  const normal = session.send({ type: 'list_tasks', request_id: 'after-pickers' });
  const outcomes = [firstPicker, secondPicker, normal].map((promise) =>
    promise.then((value) => ({ value }), (error) => ({ error })));

  try {
    const pickerTimers = [...timers.timers.values()]
      .filter((timer) => timer.delayMs >= 130_000 && timer.delayMs <= 260_000);
    assert.deepEqual(pickerTimers.map((timer) => timer.delayMs), [130_000, 260_000]);
    const normalTimer = [...timers.timers.values()].find((timer) => timer.delayMs === 280_000);
    assert.ok(normalTimer, 'normal request should wait through both queued folder pickers');

    fake.respond('picker-one', 'folder_result');
    fake.respond('picker-two', 'folder_result');
    fake.respond('after-pickers', 'task_list');
    const [firstResult, secondResult, normalResult] = await Promise.all(outcomes);
    assert.equal(firstResult.value.type, 'folder_result');
    assert.equal(secondResult.value.type, 'folder_result');
    assert.equal(normalResult.value.type, 'task_list');
    session.close();
    assert.equal(timers.timers.size, 0);
  } finally {
    session.close();
    await Promise.all(outcomes);
  }
});

test('idle session closes only after pending requests finish', async () => {
  const fake = makeFakeNativePort();
  const session = createNativeSession(fake.browser, { idleMs: 1 });
  const pending = session.send({ type: 'list_tasks' });
  await delay(5);
  assert.equal(fake.state.disconnectCalls, 0);
  fake.replyToAll();
  await pending;
  await delay(5);
  assert.equal(fake.state.disconnectCalls, 1);
});
