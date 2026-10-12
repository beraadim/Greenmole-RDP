import { useState, useRef, useEffect, useMemo } from "react";
import { useLogs } from "../hooks/useLogs";
import LogEntryRow from "../components/LogEntry";
import EmptyState from "../components/EmptyState";
import DiagnosticsExport from "../components/DiagnosticsExport";
import type { LogEntry } from "../lib/types";

type LevelFilter = "all" | LogEntry["level"];

const LEVEL_FILTERS: { key: LevelFilter; label: string }[] = [
  { key: "all", label: "All" },
  { key: "error", label: "Error" },
  { key: "warn", label: "Warn" },
  { key: "info", label: "Info" },
  { key: "debug", label: "Debug" },
];

function Logs() {
  const { logs, autoScroll, setAutoScroll, clearLogs } = useLogs();
  const [levelFilter, setLevelFilter] = useState<LevelFilter>("all");
  const [keyword, setKeyword] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  const filteredLogs = useMemo(() => {
    const kw = keyword.toLowerCase();
    return logs.filter((log) => {
      if (levelFilter !== "all" && log.level !== levelFilter) return false;
      if (kw && !log.message.toLowerCase().includes(kw)) return false;
      return true;
    });
  }, [logs, levelFilter, keyword]);

  useEffect(() => {
    if (autoScroll && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [filteredLogs, autoScroll]);

  const handleScroll = () => {
    if (!scrollRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
    const atBottom = scrollHeight - scrollTop - clientHeight < 40;
    if (autoScroll && !atBottom) {
      setAutoScroll(false);
    } else if (!autoScroll && atBottom) {
      setAutoScroll(true);
    }
  };

  const hasKeyword = keyword.trim().length > 0;

  return (
    <div className="flex flex-col gap-3 p-4 h-full">
      {/* Header row */}
      <div className="flex items-center justify-between">
        <h1 className="text-base font-semibold text-text">Logs</h1>
        <span className="flex items-center gap-1.5 text-[11px]">
          {autoScroll ? (
            <>
              <span className="text-green">●</span>
              <span className="text-text-muted">Live</span>
            </>
          ) : (
            <>
              <span className="text-text-muted">⏸</span>
              <span className="text-text-muted">Paused</span>
            </>
          )}
        </span>
      </div>

      <DiagnosticsExport />

      {/* Toolbar */}
      <div className="flex gap-1.5 items-center">
        {/* Level filter tags */}
        {LEVEL_FILTERS.map(({ key, label }) => (
          <button
            key={key}
            type="button"
            onClick={() => setLevelFilter(key)}
            className={`rounded-[5px] text-[10px] px-2 py-0.5 transition-colors ${
              levelFilter === key
                ? "bg-accent/15 border border-accent/30 text-accent"
                : "bg-card border border-border-subtle text-text-muted"
            }`}
          >
            {label}
          </button>
        ))}

        {/* Search input */}
        <input
          type="text"
          placeholder="Search logs..."
          value={keyword}
          onChange={(e) => setKeyword(e.target.value)}
          className="flex-1 bg-card border border-border rounded-[5px] text-xs px-2.5 py-1 text-text placeholder:text-text-muted outline-none focus:border-accent/50"
        />
      </div>

      {/* Log list */}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto rounded-[6px] border border-border"
        style={{ minHeight: 0 }}
      >
        {filteredLogs.length === 0 ? (
          <EmptyState
            message={hasKeyword ? "No matching logs found" : "No log entries yet"}
          />
        ) : (
          filteredLogs.map((log, i) => (
            <LogEntryRow key={`${log.timestamp}-${i}`} entry={log} />
          ))
        )}
      </div>
    </div>
  );
}

export default Logs;
