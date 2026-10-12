import test from "node:test";
import assert from "node:assert/strict";
import { recordMetricSample, sampleAverage } from "../macrdp-ui/src/lib/metricHistory.ts";
import { MetricSampleGate } from "../macrdp-ui/src/lib/metricStream.ts";

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
