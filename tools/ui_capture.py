#!/usr/bin/env python3
"""Side-by-side UI capture: run a command in a 120x36 pty, drive the same
key sequence for both builds, decode the ANSI into pyte screens, save each
frame as plain text for pixel-ish diffing.

usage: ui_capture.py OUTDIR TAG -- CMD...
"""
import fcntl
import os
import pty
import struct
import sys
import termios
import time

import pyte

COLS, ROWS = 120, 36
# (name, key-to-press-first, seconds-to-let-it-run)
SEQ = [
    ("01_dash", None, 7.0),
    ("02_manager", "g", 3.5),
    ("03_dash_back", "\x1b", 1.5),
    ("04_settings", "s", 4.0),
    ("05_dash_back", "\x1b", 1.5),
    ("06_hf", "h", 3.5),
    ("07_dash_back", "\x1b", 1.5),
    ("08_journal", "l", 5.0),
    ("09_back", "\x1b", 1.0),
]

def main() -> None:
    outdir, tag = sys.argv[1], sys.argv[2]
    cmd = sys.argv[4:]
    os.makedirs(outdir, exist_ok=True)
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLORTERM"] = "truecolor"
        os.environ.pop("LSMTOP_CONFIG", None)
        try:
            os.execvp(cmd[0], cmd)
        except Exception:
            os._exit(127)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def drain(seconds: float) -> None:
        end = time.time() + seconds
        while time.time() < end:
            import select

            r, _, _ = select.select([fd], [], [], 0.2)
            if r:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                stream.feed(data)

    for name, key, secs in SEQ:
        if key:
            os.write(fd, key.encode())
        drain(secs)
        with open(f"{outdir}/{tag}_{name}.txt", "w") as fh:
            for line in screen.display:
                fh.write(line.rstrip() + "\n")
    try:
        os.write(fd, b"q")
        time.sleep(0.4)
        os.write(fd, b"\x1b")
        time.sleep(0.3)
        os.write(fd, b"q")
        time.sleep(0.3)
    except OSError:
        pass
    os.kill(pid, 15)

if __name__ == "__main__":
    main()
