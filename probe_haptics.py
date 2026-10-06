#!/usr/bin/env python3
"""Deck haptics format probe.

Fires labeled candidate haptic reports at a USB-tethered Steam Deck
(28DE:1205) and prints an index for each — note which ones you physically
FEEL, and feed the indices back. Uses the same transport as the working
deck driver (pip hidapi send_feature_report, 65-byte report with the id at
byte 0).

Run from the repo root:
    nix develop -c python3 keebs/probe_haptics.py
"""

import sys
import time

import hid

VID, PID = 0x28DE, 0x1205
GAP = 0.7  # seconds between candidates


def make_report(report_id, body):
    r = bytearray(65)
    r[0] = report_id
    n = min(len(body), 64)
    r[1 : 1 + n] = body[:n]
    return bytes(r)


def open_devices():
    out = []
    for d in hid.enumerate(VID, PID):
        try:
            dev = hid.device()
            dev.open_path(d["path"])
            out.append((d.get("interface_number"), d.get("path"), dev))
        except Exception as e:
            print(f"  skip iface {d.get('interface_number')}: {e}")
    return out


def send_all(devs, report):
    ok = 0
    for iface, path, dev in devs:
        try:
            rc = dev.send_feature_report(report)
            if rc >= 0:
                ok += 1
                print(f"    -> iface {iface} accepted ({rc})")
        except Exception as e:
            print(f"    -> iface {iface} error: {e}")
    return ok


def main():
    devs = open_devices()
    if not devs:
        print("no 28DE:1205 HID handles opened")
        return 1
    print(f"opened {len(devs)} interface(s): {[i for i, _, _ in devs]}")

    # Disable lizard first so firmware state is like the OSK's.
    print("\n[setup] disabling lizard (0x87 setting 9)")
    send_all(devs, make_report(0x01, [0x87, 0x03, 0x09, 0x00, 0x00]))
    time.sleep(0.3)

    cands = []

    # --- S1: direct feature 0x8f, dual-motor intensity word at every offset
    for off in range(2, 9):
        body = bytearray(10)
        body[0] = 0x07
        body[1] = 0x07
        if off + 1 < 10:
            body[off] = 0xFF
            body[off + 1] = 0xFF
        cands.append((f"S1 0x8f dual words @{off}", make_report(0x8F, body)))

    # --- S2: 0x83 LFO tone (triton format), louder gains, long bursts
    for act in (0, 1, 3, 4):
        for gain in (0x7F, 0x30):
            body = bytearray(8)
            body[0] = 0x83
            body[1] = act
            body[2] = gain
            body[3] = 550 & 0xFF
            body[4] = 550 >> 8
            body[5] = 0xFF
            body[6] = 0x7F
            cands.append((f"S2 tone act{act} gain{gain:#04x}", make_report(0x83, body)))

    # --- S3: per-pad pulse encodings on 0x8f
    for pad in (0, 1, 2, 3):
        body = bytes(
            [0x07, 0x07, pad, 0x01, 0xFF, 0xFF, 0x10, 0x27, 0x08, 0x00]
        )
        cands.append((f"S3 pulse pad{pad}", make_report(0x8F, body)))

    # --- S4: classic command wrapped inside feature 0x01 (how lizard rides)
    for cmd in (0x8F, 0xB6, 0xAE):
        body = bytes([cmd, 0x07, 0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00])
        cands.append((f"S4 wrapped 0x{cmd:02x} via 0x01", make_report(0x01, body)))

    # --- S5: 0x8f with longer body (some firmwares validate length)
    for extra in (11, 13, 20):
        body = bytearray(extra)
        body[0] = 0x07
        body[1] = 0x07
        body[2] = 0x00
        body[3] = 0xFF
        body[4] = 0xFF
        body[5] = 0xFF
        body[6] = 0xFF
        cands.append((f"S5 0x8f len{extra}", make_report(0x8F, body)))

    print(f"\nfiring {len(cands)} candidates, {GAP}s apart — note the INDEX of any you FEEL\n")
    for i, (desc, report) in enumerate(cands):
        n = send_all(devs, report)
        print(f"[{i:02d}] {desc} (sent on {n} iface(s))", flush=True)
        time.sleep(GAP)

    print("\ndone. Which indices buzzed?")
    try:
        ans = input("> ")
        print("felt:", ans)
    except EOFError:
        pass

    # Restore lizard so the deck is a normal mouse again.
    print("[teardown] enabling lizard")
    send_all(devs, make_report(0x01, [0x87, 0x03, 0x09, 0x03, 0x00]))
    for _, _, dev in devs:
        try:
            dev.close()
        except Exception:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
