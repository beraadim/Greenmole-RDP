"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");

function harness() {
  let now = 0, nextFrame, paints = 0;
  const records = [], handlers = {};
  const nodes = new Map();
  const context2d = {fillRect() { paints++; }, fillText() { paints++; }};
  for (const name of ["#start", "#text", "#scroll", "#video", "#phase", "#cue", "#status", "#input"]) {
    nodes.set(name, {textContent: "", width: 960, height: 520, scrollHeight: 8000, clientHeight: 260,
      append() {}, getContext() { return context2d; }, addEventListener(event, fn) { handlers[name + event] = fn; }});
  }
  const context = vm.createContext({document: {body: {dataset: {token: "test"}}, hidden: false,
    querySelector(key) { return nodes.get(key); }, createElement() { return {}; }, addEventListener() {}},
    performance: {now() { return now; }}, Date: {now() { return 1000000 + now; }},
    innerWidth: 1920, innerHeight: 1080, devicePixelRatio: 1,
    requestAnimationFrame(fn) { nextFrame = fn; }, addEventListener() {},
    AbortSignal: {timeout() {}}, fetch(_url, options) { records.push(JSON.parse(options.body)); return Promise.resolve({ok: true}); }});
  vm.runInContext(fs.readFileSync(path.join(__dirname, "client_benchmark.js"), "utf8"), context);
  return {records, nodes, handlers, paints: () => paints,
    start() { handlers["#startclick"](); },
    async tick(ms) { now = ms; nextFrame(ms); await new Promise(resolve => setImmediate(resolve)); }};
}

test("fixed suite reaches every phase and ends at 170 seconds", async () => {
  const run = harness();
  run.start();
  for (let time = 0; time <= 170000; time += 100) await run.tick(time);
  assert.deepEqual(run.records.filter(r => r.event === "phase").map(r => r.phase),
    ["idle", "typing", "scroll", "video", "idle-after-motion", "resize", "reconnect", "slow-network"]);
  assert.equal(run.records.at(-1).event, "end");
  assert.equal(run.records.at(-1).elapsed_ms, 170000);
});

test("idle has no animated painting and typing really pauses", async () => {
  const run = harness();
  const before = run.paints();
  run.start();
  for (let time = 0; time < 8000; time += 100) await run.tick(time);
  assert.equal(run.paints(), before);
  await run.tick(12000);
  const text = run.nodes.get("#text").textContent;
  await run.tick(13900);
  assert.equal(run.nodes.get("#text").textContent, text);
  await run.tick(15000);
  assert.ok(run.nodes.get("#text").textContent.length > text.length);
});

test("manual keyboard telemetry contains counts, never typed content", async () => {
  const run = harness();
  run.start();
  await run.tick(8000);
  run.nodes.get("#input").value = "private text";
  run.handlers["#inputinput"]();
  assert.equal(run.records.at(-1).input_count, 1);
  assert.ok(!JSON.stringify(run.records).includes("private text"));
});
