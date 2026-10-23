#!/usr/bin/env python3
"""Read-only, bounded macOS observations for the explicit session soak tool.

Run in an owned subprocess: even an unresponsive native query must not hold the
observer indefinitely. No display setters, registry writes or power actions are
imported here. Identifiers are reduced to fingerprints before leaving a probe.
"""
from __future__ import annotations

import ctypes
import hashlib
import json
import math
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import time

MAX_COMMAND_BYTES = 1024 * 1024


class ProbeUnavailable(Exception):
    """A fixed operational failure, never a raw subprocess error or payload."""


def terminate_owned(process):
    """Stop only the session/process group that this function's caller created."""
    # A still-unreaped owned leader reserves its PID/PGID, including when a
    # descendant kept stdout open after that leader exited.
    if process.returncode is None:
        denied = False
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except PermissionError:
            denied = True
        # Do not reap/poll the leader during this grace period: its unreaped
        # PID reserves the owned group number even if a descendant ignores TERM.
        # A final KILL therefore cannot hit a subsequently reused unrelated PGID.
        try:
            time.sleep(.1)
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                # Darwin can reject signalling an already-exited group. Reap
                # our leader in all cases; never retry a PGID after reaping it.
                denied = True
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                return False
        return not denied
    return True


def bounded_command(argv, timeout=3, limit=MAX_COMMAND_BYTES, own_group=True):
    """Use argv only, discard stderr and bound both elapsed time and stdout."""
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, start_new_session=own_group)
    output = bytearray()
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProbeUnavailable("timeout")
            for key, _ in selector.select(min(remaining, .1)):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                if len(output) + len(data) > limit:
                    raise ProbeUnavailable("output_limit")
                output.extend(data)
        if process.wait(timeout=max(.01, deadline - time.monotonic())) != 0:
            raise ProbeUnavailable("command_failed")
        return bytes(output)
    except (OSError, subprocess.SubprocessError):
        raise ProbeUnavailable("command_unavailable") from None
    finally:
        selector.close()
        try:
            if own_group:
                terminate_owned(process)
            elif process.returncode is None:
                # Fixed ps/pmset probes have no children. They stay within the
                # outer helper's group so its timeout also removes them.
                process.kill()
                process.wait(timeout=1)
        finally:
            process.stdout.close()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def stable_identity(home, port):
    path = Path(home) / ".macrdp/display-identities" / f"rdp-port-{port}.json"
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or not 0 < info.st_size <= 4096:
            raise ProbeUnavailable("identity_unavailable")
        value = json.loads(os.read(fd, 4097))
        if (not isinstance(value, dict) or set(value) != {"schema", "serial"}
                or type(value["schema"]) is not int or value["schema"] != 1
                or type(value["serial"]) is not int or not 0 < value["serial"] < 2**32):
            raise ProbeUnavailable("identity_unavailable")
        return digest(value)
    finally:
        os.close(fd)


def process_state(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return {"process_alive": False}
    except PermissionError:
        pass
    text = bounded_command(["/bin/ps", "-axo", "pid=,ppid=,rss=,comm="], own_group=False).decode(errors="replace")
    records = []
    target_name = None
    colorsync = []
    for line in text.splitlines():
        parts = line.strip().split(None, 3)
        if len(parts) == 4 and all(value.isdigit() for value in parts[:3]):
            records.append((int(parts[0]), int(parts[1]), int(parts[2]) * 1024))
            if int(parts[0]) == pid:
                target_name = Path(parts[3]).name
    if len(records) > 32768:
        raise ProbeUnavailable("process_limit")
    owned = {pid}
    for _ in range(32):
        expanded = owned | {child for child, parent, _ in records if parent in owned}
        if expanded == owned:
            break
        owned = expanded
    else:
        raise ProbeUnavailable("process_tree_limit")
    current = next((rss for child, _, rss in records if child == pid), None)
    if current is None or target_name not in ("macrdp", "macrdp-ui", "macrdp-server"):
        raise ProbeUnavailable("process_unavailable")
    start = bounded_command(["/bin/ps", "-p", str(pid), "-o", "lstart="], own_group=False).strip()
    if not start:
        raise ProbeUnavailable("process_unavailable")
    cpu = bounded_command(["/bin/ps", "-axo", "%cpu=,comm="], own_group=False).decode(errors="replace")
    for line in cpu.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and "ColorSync" in parts[1]:
            try:
                value = float(parts[0])
                if math.isfinite(value) and value >= 0:
                    colorsync.append(value)
            except ValueError:
                pass
    return {"process_alive": True, "process_generation": hashlib.sha256(start).hexdigest(),
            "process_rss_bytes": current, "process_tree_count": len(owned),
            "process_tree_rss_bytes": sum(rss for child, _, rss in records if child in owned),
            "colorsync_process_count": len(colorsync),
            "colorsync_cpu_percent": sum(colorsync) if colorsync else None}


def wake_count(text):
    """Only macrdp's two fixed assertion reasons; never retain other app text."""
    if "Listed by owning process" not in text:
        raise ProbeUnavailable("wake_unavailable")
    return sum(bool(re.search(r'"macrdp remote (?:session active|input)"', line))
               for line in text.splitlines())


def display_state():
    cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    class Point(ctypes.Structure):
        _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]
    class Size(ctypes.Structure):
        _fields_ = [("width", ctypes.c_double), ("height", ctypes.c_double)]
    class Rect(ctypes.Structure):
        _fields_ = [("origin", Point), ("size", Size)]
    cg.CGGetOnlineDisplayList.argtypes = [ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32),
                                        ctypes.POINTER(ctypes.c_uint32)]
    cg.CGGetOnlineDisplayList.restype = ctypes.c_int32
    for name in ["CGDisplayVendorNumber", "CGDisplayModelNumber", "CGDisplaySerialNumber", "CGDisplayIsMain"]:
        getattr(cg, name).argtypes = [ctypes.c_uint32]
        getattr(cg, name).restype = ctypes.c_uint32
    for name in ["CGDisplayPixelsWide", "CGDisplayPixelsHigh"]:
        getattr(cg, name).argtypes = [ctypes.c_uint32]
        getattr(cg, name).restype = ctypes.c_size_t
    cg.CGDisplayBounds.argtypes = [ctypes.c_uint32]
    cg.CGDisplayBounds.restype = Rect
    cg.CGDisplayRotation.argtypes = [ctypes.c_uint32]
    cg.CGDisplayRotation.restype = ctypes.c_double
    ids = (ctypes.c_uint32 * 32)()
    count = ctypes.c_uint32()
    if cg.CGGetOnlineDisplayList(32, ids, ctypes.byref(count)) != 0 or not 0 < count.value < 32:
        raise ProbeUnavailable("display_unavailable")
    physical, virtual = [], 0
    for display in ids[:count.value]:
        vendor, model = cg.CGDisplayVendorNumber(display), cg.CGDisplayModelNumber(display)
        if (vendor, model) == (0x4D52, 1):
            virtual += 1
            continue
        bounds = cg.CGDisplayBounds(display)
        physical.append([vendor, model, cg.CGDisplaySerialNumber(display), cg.CGDisplayIsMain(display),
                         bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height,
                         cg.CGDisplayPixelsWide(display), cg.CGDisplayPixelsHigh(display),
                         cg.CGDisplayRotation(display)])
    return {"physical_display_count": len(physical), "physical_topology": digest(sorted(physical)),
            "virtual_display_online_count": virtual}


def registration_state():
    cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    cs = ctypes.CDLL("/System/Library/Frameworks/ColorSync.framework/ColorSync")
    cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    cf.CFDictionaryGetValue.restype = ctypes.c_void_p
    cf.CFEqual.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    cf.CFEqual.restype = ctypes.c_bool
    class UUIDBytes(ctypes.Structure):
        _fields_ = [("value", ctypes.c_ubyte * 16)]
    cf.CFUUIDGetUUIDBytes.argtypes = [ctypes.c_void_p]
    cf.CFUUIDGetUUIDBytes.restype = UUIDBytes
    device_key = ctypes.c_void_p.in_dll(cs, "kColorSyncDeviceID").value
    class_key = ctypes.c_void_p.in_dll(cs, "kColorSyncDeviceClass").value
    display_class = ctypes.c_void_p.in_dll(cs, "kColorSyncDisplayDeviceClass").value
    devices, profiles, failed = set(), [0], [False]
    callback_type = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    @callback_type
    def visit(info, _context):
        try:
            kind = cf.CFDictionaryGetValue(info, class_key)
            if not kind or not cf.CFEqual(kind, display_class):
                return True
            profiles[0] += 1
            if profiles[0] > 4096:
                failed[0] = True
                return False
            identity = cf.CFDictionaryGetValue(info, device_key)
            if not identity:
                failed[0] = True
                return False
            devices.add(bytes(cf.CFUUIDGetUUIDBytes(identity).value).hex())
            return True
        except Exception:
            failed[0] = True
            return False
    cs.ColorSyncIterateDeviceProfiles.argtypes = [callback_type, ctypes.c_void_p]
    cs.ColorSyncIterateDeviceProfiles.restype = None
    cs.ColorSyncIterateDeviceProfiles(visit, None)
    if failed[0] or not devices:
        raise ProbeUnavailable("registrations_unavailable")
    return {"monitor_registrations": len(devices), "monitor_registration_fingerprint": digest(sorted(devices)),
            "display_profile_entries": profiles[0]}


def snapshot(pid, port, home=None):
    result = {"unavailable": []}
    if sys.platform != "darwin":
        return {"unavailable": ["macos_probes"]}
    probes = {"process": lambda: process_state(pid), "displays": display_state,
              "registrations": registration_state,
              "identity": lambda: {"stable_monitor_identity": stable_identity(home or Path.home(), port)},
              "wake": lambda: {"macrdp_wake_assertions": wake_count(
                  bounded_command(["/usr/bin/pmset", "-g", "assertions"], own_group=False).decode(errors="replace"))}}
    for name, probe in probes.items():
        try:
            result.update(probe())
        except (OSError, ValueError, TypeError, AttributeError, ProbeUnavailable):
            result["unavailable"].append(name)
    return result


if __name__ == "__main__":
    # This internal subprocess entrypoint accepts only bounded numeric selectors.
    try:
        pid, port = map(int, sys.argv[1:])
        if not 0 < pid < 2**31 or not 0 < port <= 65535:
            raise ValueError()
        print(json.dumps(snapshot(pid, port), allow_nan=False, separators=(",", ":")))
    except (ValueError, TypeError):
        raise SystemExit(2)
