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
    disconnect() {
      state.onDisconnect();
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
