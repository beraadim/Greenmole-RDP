import type { Metrics } from "../lib/types";

const duration = (value: number | null) => value === null ? "—" : value.toFixed(1);

export default function TimingDetails({ metrics }: { metrics: Metrics }) {
  const rows = [
    { label: "Capture queue", timing: metrics.capture_timings },
    { label: "Encoding", timing: metrics.encode_timings },
    { label: "Encode delivery", timing: metrics.completion_timings },
    { label: "Socket write", timing: metrics.write_timings },
  ];
  return (
    <div className="overflow-x-auto mb-3">
      <table className="w-full text-[11px] text-right">
        <caption className="text-left text-text-muted mb-2">
          Recent frame and update timings (ms). Up to 1,024 events per stage; not end-to-end latency.
        </caption>
        <thead className="text-text-muted">
          <tr><th className="text-left font-medium">Stage</th><th>Samples</th><th>p50</th><th>p95</th><th>p99</th><th>Worst</th></tr>
        </thead>
        <tbody>
          {rows.map(({ label, timing }) => (
            <tr key={label} className="border-t border-border-subtle">
              <th className="text-left font-normal py-1">{label}</th>
              <td>{timing.samples}</td><td>{duration(timing.p50_ms)}</td>
              <td>{duration(timing.p95_ms)}</td><td>{duration(timing.p99_ms)}</td><td>{duration(timing.max_ms)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
