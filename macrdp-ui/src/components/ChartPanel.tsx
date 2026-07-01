import type { ReactNode } from "react";

interface ChartPanelProps {
  title: string;
  value?: string;
  valueColor?: string;
  empty?: boolean;
  emptyText?: string;
  children: ReactNode;
}

export function ChartPanel({
  title,
  value,
  valueColor,
  empty,
  emptyText = "Waiting for data...",
  children,
}: ChartPanelProps) {
  return (
    <div className="bg-card rounded-[8px] p-3 flex-1 min-w-0">
      {/* Header */}
      <div className="flex items-center justify-between mb-2">
        <span className="text-[11px] text-text-muted">{title}</span>
        {value !== undefined && (
          <span
            className="text-[11px] font-medium"
            style={valueColor ? { color: valueColor } : undefined}
          >
            {value}
          </span>
        )}
      </div>

      {/* Body */}
      {empty ? (
        <div className="relative" style={{ height: 60 }}>
          {/* Dashed baseline */}
          <div className="absolute bottom-0 left-0 right-0 border-t border-dashed border-border" />
          {/* Centered text */}
          <div className="absolute inset-0 flex items-center justify-center">
            <span className="text-[11px] text-text-muted">{emptyText}</span>
          </div>
        </div>
      ) : (
        children
      )}
    </div>
  );
}
