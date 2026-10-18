import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { recordMetricSample, sampleAverage } from "../macrdp-ui/src/lib/metricHistory.ts";
import { MetricSampleGate } from "../macrdp-ui/src/lib/metricStream.ts";
import { hasLiveSessionMetrics } from "../macrdp-ui/src/lib/metricPresentation.ts";

const point = (sampleId, timestampMs, value) => ({ sampleId, timestampMs, value });

test("stopping or unmounting invalidates a poll even with an unseen newer sample", () => {
  const gate = new MetricSampleGate();
  assert.ok(gate.admit(10));
  const beforeStop = gate.epoch;
  gate.reset();
  assert.equal(gate.admit(11, beforeStop), false);
  assert.ok(gate.admit(12));
  assert.equal(gate.admit(11), false);
  const beforeUnmount = gate.epoch;
  gate.reset();
  assert.equal(gate.admit(13, beforeUnmount), false);
});

test("steady values and idle zeros count as samples in the average", () => {
  let history = [];
  for (let id = 1; id <= 10; id++) history = recordMetricSample(history, point(id, id * 1000, 8));
  history = recordMetricSample(history, point(11, 11000, 0));
  assert.equal(history.length, 11);
  assert.equal(sampleAverage(history), 80 / 11);
});

test("poll duplicates and delayed results cannot overwrite event history", () => {
  const history = recordMetricSample([], point(2, 2000, 8));
  assert.equal(recordMetricSample(history, point(2, 2000, 99)), history);
  assert.equal(recordMetricSample(history, point(1, 1000, 99)), history);
});

test("history retains actual timing and expires by time rather than changes", () => {
  let history = recordMetricSample([], point(1, 1000, 4));
  history = recordMetricSample(history, point(2, 3000, 4));
  history = recordMetricSample(history, point(3, 62000, 4));
  assert.deepEqual(history.map(p => p.timestampMs), [3000, 62000]);
});

test("clock corrections, stop, and invalid metrics do not corrupt the chart", () => {
  const history = recordMetricSample([], point(1, 10000, 4));
  assert.equal(recordMetricSample(history, point(2, 11000, NaN)), history);
  assert.equal(recordMetricSample(history, point(0, 11000, 4)), history);
  assert.deepEqual(recordMetricSample(history, point(2, 9000, 4)), [point(2, 9000, 4)]);
  assert.deepEqual(recordMetricSample(history, null), []);
  assert.equal(sampleAverage([]), null);
});

test("idle heartbeats cannot append retained encoding values to the chart", () => {
  const active = { session_active: true, sample_id: 1, timestamp_ms: 1000, encode_ms: 3 };
  let history = recordMetricSample([], point(1, 1000, active.encode_ms));

  // The disconnect event can precede an idle metrics tick. Both must hide the
  // old frame timing, including servers predating the session_active field.
  assert.equal(hasLiveSessionMetrics(active, true, 0, false), false);
  assert.equal(hasLiveSessionMetrics({ ...active, session_active: undefined }, true, 0, false), false);
  for (let id = 2; id <= 5; id++) {
    const idle = { ...active, session_active: false, sample_id: id, timestamp_ms: id * 1000 };
    const available = hasLiveSessionMetrics(idle, true, 0, false);
    history = recordMetricSample(history, available ? point(id, id * 1000, idle.encode_ms) : null);
  }
  assert.deepEqual(history, []);
  assert.equal(sampleAverage(history), null);

  // Reconnecting does not revive the previous session's idle snapshot.
  assert.equal(hasLiveSessionMetrics({ ...active, session_active: false }, true, 1, false), false);
  assert.equal(hasLiveSessionMetrics(active, true, 1, false), true);
  assert.equal(hasLiveSessionMetrics(active, false, 1, false), false);
  assert.equal(hasLiveSessionMetrics(active, true, 1, true), false);
  assert.equal(hasLiveSessionMetrics(null, true, 1, false), false);
  assert.equal(hasLiveSessionMetrics({ ...active, session_active: undefined }, true, 1, false), true);
});

/** Exercise the production hook without a webview or a real polling timer. */
function connectionsHarness() {
  const require = createRequire(import.meta.url);
  const ts = require("../macrdp-ui/node_modules/typescript");
  const source = readFileSync(new URL("../macrdp-ui/src/hooks/useConnections.ts", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2021 },
  }).outputText;
  const pending = [];
  let state;
  let listener;
  let poll;
  let cleanup;
  let unlistened = false;
  const react = {
    useState(initial) {
      state = initial;
      return [state, value => { state = typeof value === "function" ? value(state) : value; }];
    },
    useRef: current => ({ current }),
    useCallback: callback => callback,
    useEffect: callback => { cleanup = callback(); },
  };
  const dependencies = {
    react,
    "@tauri-apps/api/event": {
      listen: (_event, callback) => {
        listener = callback;
        return Promise.resolve(() => { unlistened = true; });
      },
    },
    "../lib/ipc": {
      api: {
        getConnections: () => new Promise(resolve => { pending.push({ resolve }); }),
      },
    },
  };
  const exports = {};
  const evaluate = new Function("require", "exports", "setInterval", "clearInterval", compiled);
  evaluate(name => {
    assert.ok(name in dependencies, `Unexpected production hook dependency: ${name}`);
    return dependencies[name];
  }, exports, callback => { poll = callback; return 1; }, () => { poll = null; });
  exports.useConnections();
  return {
    pending,
    get state() { return state; },
    get unlistened() { return unlistened; },
    get polling() { return poll !== null; },
    event: payload => listener({ payload }),
    poll: () => poll(),
    unmount: () => cleanup(),
  };
}

test("a delayed populated connection poll cannot undo a disconnect event", async () => {
  const hook = connectionsHarness();
  const connected = [{ client_name: "test", connected_at: "2026-10-18T12:00:00Z" }];
  hook.event(connected);
  hook.poll();
  hook.event([]);
  hook.pending[0].resolve(connected);
  hook.pending[1].resolve(connected);
  await Promise.resolve();
  assert.deepEqual(hook.state, []);
  hook.unmount();
});

test("connection fallback polls accept only the latest snapshot and cancel on unmount", async () => {
  const hook = connectionsHarness();
  const connected = [{ client_name: "test", connected_at: "2026-10-18T12:00:00Z" }];
  hook.poll();
  hook.pending[1].resolve([]);
  await Promise.resolve();
  hook.pending[0].resolve(connected);
  await Promise.resolve();
  assert.deepEqual(hook.state, []);

  hook.poll();
  hook.pending[2].resolve(connected);
  await Promise.resolve();
  assert.deepEqual(hook.state, connected);

  hook.poll();
  hook.unmount();
  hook.pending[3].resolve([]);
  await Promise.resolve();
  assert.deepEqual(hook.state, connected);
  assert.equal(hook.polling, false);
  assert.equal(hook.unlistened, true);
});
