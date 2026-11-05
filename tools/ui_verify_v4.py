#!/usr/bin/env python3
"""v4 UI verification: every bottom hotkey strip is clickable by mouse,
open settings fields highlight. Usage: ui_verify_v4.py OUTDIR TAG -- env K=V ... -- cmd...
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

W, H = 120, 36
screen = pyte.Screen(W, H)
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


def dump(name):
    with open(f"{OUT}/{TAG}_{name}.txt", "w") as f:
        f.write("\n".join(snap()) + "\n")


def find(lines, needle):
    for i, ln in enumerate(lines):
        if needle in ln:
            return i
    return -1


def token_pos(needle):
    """first (row, col) 1-based where needle appears on screen"""
    for i, ln in enumerate(screen.display):
        j = ln.find(needle)
        if j >= 0:
            return i + 1, j + 3  # 2 chars inside the token
    return None


def click(child, row, col):  # 1-based SGR
    send(child, f"\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m".encode())
    drain(child, 0.6)


def click_token(child, needle):
    pos = token_pos(needle)
    assert pos, f"token {needle!r} not on screen"
    click(child, pos[0], pos[1])
    return pos


fails = []


def expect(name, cond, extra=""):
    print(("PASS " if cond else "FAIL ") + name + (" " + extra if extra and not cond else ""))
    if not cond:
        fails.append(name)


def main():
    os.makedirs(OUT, exist_ok=True)
    env = dict(os.environ, TERM="xterm-256color", COLORTERM="default",
               **env_extra)
    pid, child = pty.fork()
    if pid == 0:
        os.execvpe(CMD[0], CMD, env)
    import fcntl, struct, termios
    fcntl.ioctl(child, termios.TIOCSWINSZ, struct.pack("HHHH", H, W, 0, 0))
    time.sleep(1.6)
    drain(child, 5.0)
    dump("01_dash")

    # 1) click "s Settings" on the dashboard strip
    click_token(child, "s Settings")
    dump("02_settings")
    ln = snap()
    expect("dash_strip_settings_click", find(ln, "Metrics options") >= 0,
           "no settings screen after strip click")

    # 2) focused field label is highlighted (cyan)
    row = find(ln, "llama-server binary")
    if row < 0:
        row = find(ln, "Models directory")
    if row >= 0:
        cells = [screen.buffer[row][c] for c in range(W)
                 if screen.buffer[row][c].data not in (" ", "")]
        fgs = {c.fg for c in cells}
        # cyan: SGR-16 "6", 256 "00aaaa"/ANSI cyan, or RGB #00CDCD
        cyan = {"6", "00aaaa", "00cdcd", "00ffd7", "51ffff"}
        expect("focused_label_cyan", bool(fgs & cyan), f"fgs={fgs}")
    else:
        expect("focused_label_row_found", False)

    # 3) Enter opens the field -> the value row gets a REVERSE highlight bar
    send(child, b"\r")
    drain(child, 0.7)
    dump("03_edit")
    ln = snap()
    rev_ok = False
    for r in range(H):
        revs = sum(1 for c in range(W) if getattr(screen.buffer[r][c], "reverse", False))
        if revs >= 10:  # a full-width bar, not an incidental cell
            rev_ok = True
            break
    expect("open_field_highlight_bar", rev_ok, "no reverse-video bar on the edit row")

    # typing must go to the field, not the app (letter q is a hotkey)
    send(child, b"z")
    drain(child, 0.4)
    alive = True
    try:
        p, _ = os.waitpid(pid, os.WNOHANG)
        alive = p == 0
    except ChildProcessError:
        alive = False
    expect("typing_in_field_not_swallowed_by_hotkeys", alive)
    send(child, b"\x7f")  # backspace the z back off
    drain(child, 0.3)
    send(child, b"\x1b")  # esc closes the field (no commit change)
    drain(child, 0.5)

    # 4) click "m Metrics options" in the settings hint line
    click_token(child, "m Metrics options")
    dump("04_opts")
    ln = snap()
    expect("settings_hint_m_click", find(ln, "Save(enter)") >= 0,
           "opts screen not opened via hint click")

    # 5) click Cancel(esc) -> back to settings
    click_token(child, "Cancel(esc)")
    dump("05_back_to_settings")
    ln = snap()
    expect("opts_cancel_click", find(ln, "Metrics options") >= 0,
           "cancel did not return to settings")

    # 6) click esc Back -> dashboard
    click_token(child, "esc Back")
    dump("06_dash_again")
    ln = snap()
    expect("settings_esc_click_back", find(ln, "g Llama") >= 0,
           "did not return to dashboard")

    # 7) click "g Llama" -> manager
    click_token(child, "g Llama")
    dump("07_manager")
    ln = snap()
    expect("dash_strip_llama_click", find(ln, "r Refresh") >= 0,
           "manager not opened via strip click")

    # 8) manager footer esc Back click -> dashboard
    click_token(child, "esc Back")
    dump("08_dash_final")
    ln = snap()
    expect("manager_esc_click", find(ln, "q Quit") >= 0)

    # 9) clicking "q Quit" quits for real
    click_token(child, "q Quit")
    time.sleep(0.8)
    quit_ok = False
    try:
        p, _ = os.waitpid(pid, os.WNOHANG)
        quit_ok = p == pid
    except ChildProcessError:
        quit_ok = True
    expect("strip_quit_click_quits", quit_ok)
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except (ProcessLookupError, ChildProcessError):
        pass

    print("FRAMES in", OUT)
    if fails:
        print("FAILS:", fails)
        sys.exit(1)
    print("ALL ASSERTIONS PASS")


main()
