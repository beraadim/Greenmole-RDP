#!/usr/bin/env python3
import json
import re
import sys
from pathlib import Path


PAIR_RE = re.compile(r"([a-zA-Z_]+)=([^ ]+)")


def parse_pairs(message: str) -> dict[str, str]:
    return {key: value for key, value in PAIR_RE.findall(message)}


def load_entries(path: Path):
    summaries = []
    events = []
    with path.open("r", encoding="utf-8") as handle:
        for raw in handle:
            raw = raw.strip()
            if not raw:
                continue
            try:
                entry = json.loads(raw)
            except json.JSONDecodeError:
                continue
            message = entry.get("message", "")
            if "SESSION_AUDIT " not in message:
                continue
            fields = parse_pairs(message)
            kind = "other"
            if "SESSION_AUDIT summary" in message:
                kind = "summary"
                summaries.append((entry, fields))
            elif "SESSION_AUDIT path-" in message:
                kind = "path"
            elif "SESSION_AUDIT fallback-reason" in message:
                kind = "fallback"
            events.append((kind, entry, fields, message))
    return summaries, events


def as_int(fields: dict[str, str], key: str) -> int:
    try:
        return int(fields.get(key, "0"))
    except ValueError:
        return 0


def as_float(fields: dict[str, str], key: str) -> float:
    try:
        return float(fields.get(key, "0"))
    except ValueError:
        return 0.0


def print_summary(path: Path, summaries, events):
    print(f"log: {path}")
    print(f"session_audit_events: {len(events)}")
    if not summaries:
        print("no SESSION_AUDIT summary entries found")
        return 1

    for entry, fields in summaries:
        session_id = fields.get("session_id", "?")
        peer = fields.get("peer", "unknown")
        duration_ms = as_int(fields, "duration_ms")
        total_frames = as_int(fields, "total_frames")
        total_bytes = as_int(fields, "total_bytes")
        first_path = fields.get("first_path", "none")
        final_path = fields.get("final_path", "none")
        path_switches = as_int(fields, "path_switches")
        avc_frames = as_int(fields, "avc_frames")
        avc_bytes = as_int(fields, "avc_bytes")
        avc_avg = as_float(fields, "avc_avg_encode_ms")
        avc_max = as_float(fields, "avc_max_encode_ms")
        unc_frames = as_int(fields, "uncompressed_frames")
        unc_bytes = as_int(fields, "uncompressed_bytes")
        bmp_frames = as_int(fields, "bitmap_frames")
        bmp_bytes = as_int(fields, "bitmap_bytes")
        fallback_reason = fields.get("fallback_reason", "none")
        max_dirty_rects = as_int(fields, "max_dirty_rects")
        max_dirty_pixels = as_int(fields, "max_dirty_pixels")

        def pct(n: int) -> str:
            if total_frames <= 0:
                return "0.0%"
            return f"{(n * 100.0 / total_frames):.1f}%"

        print()
        print(
            f"session {session_id}: peer={peer} duration={duration_ms}ms "
            f"frames={total_frames} bytes={total_bytes}"
        )
        print(
            f"  paths: first={first_path} final={final_path} switches={path_switches} "
            f"fallback_reason={fallback_reason}"
        )
        print(
            f"  avc: frames={avc_frames} ({pct(avc_frames)}) bytes={avc_bytes} "
            f"avg_encode_ms={avc_avg:.2f} max_encode_ms={avc_max:.2f}"
        )
        print(
            f"  uncompressed: frames={unc_frames} ({pct(unc_frames)}) bytes={unc_bytes}"
        )
        print(f"  bitmap: frames={bmp_frames} ({pct(bmp_frames)}) bytes={bmp_bytes}")
        print(
            f"  damage: max_dirty_rects={max_dirty_rects} max_dirty_pixels={max_dirty_pixels}"
        )

    return 0


def main() -> int:
    default_path = Path("/tmp/macrdp-logs.jsonl")
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else default_path
    if not path.exists():
        print(f"log file not found: {path}", file=sys.stderr)
        return 2
    summaries, events = load_entries(path)
    return print_summary(path, summaries, events)


if __name__ == "__main__":
    raise SystemExit(main())
