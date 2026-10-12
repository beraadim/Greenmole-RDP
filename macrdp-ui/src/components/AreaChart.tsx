import { useId } from "react";
import { HISTORY_WINDOW_MS, type MetricPoint } from "../lib/metricHistory";

interface AreaChartProps {
  data: MetricPoint[];
  color: string;
  height?: number;
  maxValue?: number;
}

export function AreaChart({ data, color, height = 60, maxValue }: AreaChartProps) {
  const id = useId();
  const gradientId = `area-grad-${id}`;

  if (data.length === 0) return null;

  const max = maxValue ?? Math.max(...data.map(p => p.value), 1);
  const end = data[data.length - 1].timestampMs;

  // Build points: x in [0, 100], y in [0, 100] (0 = top)
  const points = data.map(p => {
    const x = Math.max(0, 100 - (end - p.timestampMs) / HISTORY_WINDOW_MS * 100);
    const y = 100 - (p.value / max) * 100;
    return { x, y };
  });

  const linePath = points.map((p, i) => `${i === 0 ? "M" : "L"}${p.x},${p.y}`).join(" ");
  const areaPath = `${linePath} L100,100 L${points[0].x},100 Z`;

  return (
    <svg
      viewBox="0 0 100 100"
      preserveAspectRatio="none"
      width="100%"
      height={height}
      className="block"
    >
      <defs>
        <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity={0.3} />
          <stop offset="100%" stopColor={color} stopOpacity={0} />
        </linearGradient>
      </defs>
      <path d={areaPath} fill={`url(#${gradientId})`} />
      <path d={linePath} stroke={color} strokeWidth="1.5" fill="none" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}
