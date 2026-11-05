#!/usr/bin/env python3
"""v3 UI verification: settings ring, edit mode, m hotkey, opts up/down,
mouse clicks (raw SGR injected). Usage: ui_verify_v3.py OUTDIR TAG [-- env K=V ...] -- cmd...
"""
import os, pty, select, sys, time

OUT, TAG = sys.argv[1], sys.argv[2]
rest = sys.argv[3:]
env_extra = {}
if "--" in rest:
    i = rest.index("--")
    for kv in rest[:i]:
        k, v = kv.split("=", 1)
        env_extra[k] = v
    CMD = rest[i + 1:]
else:
    CMD = rest

import pyte
screen = pyte.Screen(120, 36)
stream = pyte.ByteStream(screen)


def snap():
    return [ln.rstrip() for ln in screen.display]


def send(child, data):
    if isinstance(data, str):
        data = data.encode()
    os.write(child, data)


def drain(child, secs):
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([child], [], [], 0.05)
        if r:
            try:
                b = os.read(child, 65536)
            except OSError:
                break
            if not b:
                break
            stream.feed(b)


def click(child, col, row):  # 1-based SGR
    send(child, f"\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m".encode())


def find(lines, needle):
    for i, ln in enumerate(lines):
        if needle in ln:
            return i
    return -1


def main():
    os.makedirs(OUT, exist_ok=True)
    env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", **env_extra)
    pid, child = pty.fork()
    if pid == 0:
        os.environ.update(env)
        try:
            os.execvp(CMD[0], CMD)
        except Exception:
            os._exit(127)
    # rows, cols
    import fcntl, struct, termios
    fcntl.ioctl(child, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    fails = []

    def frame(name, must=(), must_not=(), ms=0.0):
        drain(child, ms or 1.2)
        lines = snap()
        with open(f"{OUT}/{TAG}_{name}.txt", "w") as f:
            f.write("\n".join(lines))
        for s in must:
            if not any(s in ln for ln in lines):
                fails.append(f"{name}: MISSING {s!r}")
        for s in must_not:
            if any(s in ln for ln in lines):
                fails.append(f"{name}: PRESENT {s!r}")

    def key(name, data, **kw):
        send(child, data)
        frame(name, **kw)

    drain(child, 6.0)
    frame("01_dash",
          must=["a Lanes"],
          must_not=["Quit+Stop", "Quit+Side"])  # router-only: no side launcher hint
    key("02_settings", b"s",
        must=["Metrics graph options", "(m)", "enter Edit/Open"],
        ms=2.0)
    key("03_ring_wrap_up", b"\x1b[A",  # Up from field 0 wraps to metrics row
        must=["\u25b8 Metrics graph options", "(enter)"])
    key("04_opts_open", b"\r",  # enter opens the options screen
        must=["chars:", "braille", "\u2191\u2193 row"],
        must_not=["chars: chars"],
        ms=1.6)
    key("05_opts_down", b"\x1b[B", must=["fill"])
    key("06_opts_up", b"\x1b[A")
    key("07_back_settings", b"\x1b", ms=1.4, must=["Settings"])
    # focus is still on the metrics row (3): Down wraps to field 0; enter opens it
    key("08_field0_focus", b"\x1b[B")
    key("09_edit_open", b"\r", must=["\u258a"])  # caret = field OPEN
    # letters are swallowed ONLY now; even q goes into the field, not the app
    key("10_edit_q_goes_in", b"q", must=["llama-serverq", "\u258a"], ms=0.8)
    key("11_edit_backspace", b"\x7f", must=["llama-server"], ms=0.8)
    key("12_leave_edit", b"\x1b", must=["Settings"], must_not=["\u258a"])
    # outside edit mode letters are HOTKEYS: 'x' must not touch the field
    key("13_hotkeys_safe", b"xjklz", must=["llama-server "], must_not=["llama-server x", "llama-serverx"])
    # and 'm' opens the options again
    key("14_m_opens_opts", b"m", must=["Metrics \u2014 chips apply live", "gpu_graph"], ms=1.6)
    # MOUSE: click the "block" chip on the first row (geometry: label col 12,
    # chips from x=15 1-based; "[ braille ]" occupies cols 15..25, gap 26,
    # "[ block ]" 27..35; row 0 chips sit on screen row 3 (header+title+0))
    ln = snap()
    row = find(ln, "braille")
    if row >= 0:
        click(child, 28, row + 1)
        frame("15_mouse_chip", must=["block"], ms=1.2)
        ch = screen.buffer[row][27]  # middle of "[ block ]"
        rev = "reverse" in (getattr(ch, "fg", "") + getattr(ch, "bg", "")) or getattr(ch, "reverse", False)
        if not rev:
            fails.append(f"15_mouse_chip: clicked chip not active (fg={getattr(ch, 'fg', None)} bg={getattr(ch, 'bg', None)})")
    key("16_opts_cancel", b"\x1b", must=["Settings"])
    # MOUSE: click the metrics options row from settings
    ln = snap()
    row = find(ln, "Metrics graph options")
    if row >= 0:
        click(child, 12, row + 1)
        frame("17_mouse_metrics_row", must=["chips apply live"], ms=1.6)
    key("18_back", b"\x1b\x1b", ms=1.2)
    # MOUSE: click a field box on settings → focus + edit mode (caret)
    key("19_settings_again", b"s", must=["Settings"], ms=1.6)
    ln = snap()
    row = find(ln, "Models directory")
    if row >= 0:
        click(child, 8, row + 2 + 1)  # box line below the label (1-based)
        frame("20_mouse_field", must=["\u258a"], ms=1.0)
    # typing now goes to the models field
    key("21_type_models", b"_zz", must=["_zz"], ms=0.8)
    key("22_leave", b"\x1b\x1b\x1b", ms=1.0)
    send(child, b"q")
    drain(child, 1.5)
    try:
        os.close(child)
    except OSError:
        pass
    try:
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        pass
    print("FRAMES in", OUT)
    if fails:
        print("FAILS:")
        for f in fails:
            print("  -", f)
        sys.exit(1)
    print("ALL ASSERTIONS PASS")


main()
