"use strict";
// All scene positions/text depend on elapsed time and this fixed seed, not on
// the number of frames delivered. Dropped rAF callbacks cannot slow the scene.
const seed = 12345678;
const phases = [
  ["idle", 8, "Leave the desktop untouched; observe idle bandwidth."],
  ["typing", 24, "Four-second typing bursts alternate with two-second pauses. Optional: type the same test sentence in the input box."],
  ["scroll", 20, "Deterministic document scrolling. Check text clarity and stale regions."],
  ["video", 30, "Synthetic 30 fps motion (not a media decoder benchmark). Check smoothness and image quality."],
  ["idle-after-motion", 8, "Leave the desktop untouched; the final image must remain complete."],
  ["resize", 20, "MANUAL: resize the RDP client at +5s and restore its original size at +12s. Check borders, pointer alignment and final size."],
  ["reconnect", 30, "MANUAL: disconnect at +5s; reconnect at +15s. Keep this host browser open. Check the recovered desktop."],
  ["slow-network", 30, "MANUAL: use the preconfigured network-limited path documented in metadata. This page does not shape traffic."],
];
const startButton = document.querySelector("#start");
const text = document.querySelector("#text");
const scroll = document.querySelector("#scroll");
const canvas = document.querySelector("#video");
const ctx = canvas.getContext("2d");
const token = document.body.dataset.token;
const words = "The quick brown fox checks sharp text, smooth scrolling, and reliable remote desktop recovery. 0123456789\n".repeat(80);
let started = null, phaseIndex = -1, phase = "idle", ended = false;
let lastFrame = null, lastSample = 0, lastPaint = -1, lastTextLength = -1;
let frameIntervals = [], inputCount = 0, pendingPosts = 0;
for (let i = 0; i < 200; i++) {
  const row = document.createElement("div");
  row.className = "row";
  row.textContent = `Row ${String(i).padStart(3, "0")} — ${(i * 7919 + seed) % 100000} — deterministic document`;
  scroll.append(row);
}

function emit(event, details = {}) {
  // Bound outstanding telemetry if the local collector stalls. Rendering must
  // continue independently, and an incomplete run must be visibly identified.
  if (pendingPosts >= 4) {
    document.querySelector("#status").textContent = "Telemetry backlog: this run has missing samples.";
    return;
  }
  pendingPosts++;
  fetch("/event", {method: "POST", headers: {"Content-Type": "application/json", "X-Benchmark-Token": token},
    body: JSON.stringify({event, phase, timestamp_ms: Date.now(), elapsed_ms: started === null ? 0 : performance.now() - started, ...details}),
    signal: AbortSignal.timeout(3000)})
    .then(response => { if (!response.ok) throw new Error("collector rejected sample"); })
    .catch(() => { document.querySelector("#status").textContent = "Collector unavailable: this run has missing samples."; })
    .finally(() => { pendingPosts--; });
}

function viewport() {
  return {width: innerWidth, height: innerHeight, device_pixel_ratio: devicePixelRatio};
}
function quantile(values, percentile) {
  return values[Math.max(0, Math.ceil(values.length * percentile) - 1)];
}
function motion(seconds) {
  ctx.fillStyle = "#173047";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  for (let i = 0; i < 80; i++) {
    const x = ((i * 137 + seconds * (55 + i % 7 * 10)) % 1100) - 70;
    const y = 260 + Math.sin(seconds * 1.4 + i * .7) * 220;
    ctx.fillStyle = `hsl(${(i * 37 + seed) % 360} 68% 62%)`;
    ctx.fillRect(x, y, 65, 44);
  }
  ctx.fillStyle = "white";
  ctx.font = "28px monospace";
  ctx.fillText(`source frame ${Math.floor(seconds * 30)}`, 24, 42);
}
function tick(now) {
  if (ended) return;
  const elapsed = (now - started) / 1000;
  let boundary = 0, index = 0;
  while (index < phases.length && elapsed >= boundary + phases[index][1]) boundary += phases[index++][1];
  if (index === phases.length) {
    emit("end", viewport());
    ended = true;
    document.querySelector("#phase").textContent = "Complete — stop collector, save manual observations, then compare runs.";
    return;
  }
  if (index !== phaseIndex) {
    phaseIndex = index;
    phase = phases[index][0];
    document.querySelector("#phase").textContent = `${index + 1}/${phases.length}: ${phase} — ${phases[index][1]} seconds`;
    document.querySelector("#cue").textContent = phases[index][2];
    frameIntervals = [];
    lastFrame = null;
    lastPaint = -1;
    emit("phase", viewport());
  }
  const local = elapsed - boundary;
  const frame = Math.floor(local * 30);
  if (phase === "typing") {
    const count = Math.floor(local / 6) * 80 + Math.floor(Math.min(local % 6, 4) * 20);
    if (count !== lastTextLength) { text.textContent = words.slice(0, count); text.scrollTop = text.scrollHeight; lastTextLength = count; }
  } else if (phase === "scroll") {
    scroll.scrollTop = Math.floor(local * 180) % (scroll.scrollHeight - scroll.clientHeight);
  } else if (["video", "resize", "reconnect", "slow-network"].includes(phase) && frame !== lastPaint) {
    motion(frame / 30);
    lastPaint = frame;
  }
  if (lastFrame !== null) frameIntervals.push(now - lastFrame);
  lastFrame = now;
  if (now - lastSample >= 1000 && frameIntervals.length) {
    frameIntervals.sort((a, b) => a - b);
    emit("render", {frames: frameIntervals.length, p50_ms: quantile(frameIntervals, .5), p95_ms: quantile(frameIntervals, .95), p99_ms: quantile(frameIntervals, .99)});
    frameIntervals = [];
    lastSample = now;
  }
  requestAnimationFrame(tick);
}
startButton.addEventListener("click", () => {
  if (started !== null) return;
  startButton.disabled = true;
  started = performance.now();
  lastSample = started;
  emit("start", {...viewport(), seed});
  requestAnimationFrame(tick);
});
addEventListener("resize", () => { if (started !== null && !ended) emit("resize", viewport()); });
document.addEventListener("visibilitychange", () => { if (started !== null && !ended) emit("visibility", {visible: document.hidden ? 0 : 1}); });
document.querySelector("#input").addEventListener("input", () => { if (started !== null && !ended) emit("input", {input_count: ++inputCount}); });
motion(0);
