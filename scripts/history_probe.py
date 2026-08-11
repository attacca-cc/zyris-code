#!/usr/bin/env python3
"""Opens an existing session and prints the transcript, so a person can look at it.

**No turn is started, so no credits are spent.** The work cards on screen come from the server's
own history replay, which is the only way to see the card shape against real events — a hand-made
`Item` proves the renderer, not what actually arrives.

This is a probe, not a check: it prints screens and judges nothing. It is the same PTY harness the
smoke scripts use, and it inherits their two hard-won rules — set the window size (the pty
`openpty()` creates is 0×0 and ratatui then emits nothing), and search the *accumulated* buffer,
because ratatui only sends the cells that changed.

Usage: python3 scripts/history_probe.py [session-index]
"""

import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

BIN = "target/debug/zyris-code"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")
ROWS, COLS = 44, 110

# Keys, as the terminal sends them.
LEFT = b"\x1b[D"
DOWN = b"\x1b[B"
ENTER = b"\r"
CTRL_O = b"\x0f"
CTRL_L = b"\x0c"
CTRL_C = b"\x03"


def drain(fd, buf, seconds):
    """Reads for `seconds`, appending to `buf`. Returns nothing — the caller renders `buf`."""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.3)
        if not r:
            continue
        try:
            chunk = os.read(fd, 1 << 16)
        except OSError:
            return
        if not chunk:
            return
        buf.append(chunk.decode("utf-8", "replace"))


def wait_for(fd, needle, buf, seconds, label):
    end = time.time() + seconds
    while True:
        if needle in ANSI.sub("", "".join(buf)):
            return True
        if time.time() >= end:
            print(f"  ✗ {label}: never saw {needle!r}", file=sys.stderr)
            return False
        drain(fd, buf, 0.5)


def screen(buf):
    """Replays the cursor-addressed output into a grid, so what is printed is what is on screen.

    ratatui writes `ESC[row;colH` then the cells that changed, so concatenating the stream gives
    a jumble. Only a replay shows the actual screen.
    """
    grid = [[" "] * COLS for _ in range(ROWS)]
    row = col = 0
    stream = "".join(buf)
    i = 0
    while i < len(stream):
        ch = stream[i]
        if ch == "\x1b":
            m = ANSI.match(stream, i)
            if not m:
                i += 1
                continue
            seq = m.group(0)
            if seq.endswith("H"):
                parts = seq[2:-1].split(";")
                row = int(parts[0] or 1) - 1
                col = int(parts[1] or 1) - 1 if len(parts) > 1 else 0
            elif seq.endswith("J"):
                grid = [[" "] * COLS for _ in range(ROWS)]
            elif seq.endswith("C"):
                col += int(seq[2:-1] or 1)
            elif seq.endswith("K"):
                # Erase to end of line. Without this a panel that closed stays on the grid and
                # every later screen reads as two frames stacked on each other.
                for c in range(col, COLS):
                    grid[row][c] = " "
            i = m.end()
            continue
        if ch == "\n":
            row, col = row + 1, 0
        elif ch == "\r":
            col = 0
        elif ch >= " ":
            if 0 <= row < ROWS and 0 <= col < COLS:
                grid[row][col] = ch
            # Fullwidth glyphs own the cell behind them; nothing is ever written there.
            col += 2 if ord(ch) > 0x1100 and _wide(ch) else 1
        i += 1
    return "\n".join("".join(r).rstrip() for r in grid)


def _wide(ch):
    import unicodedata

    return unicodedata.east_asian_width(ch) in ("W", "F")


def repaint(fd, buf, seconds=3):
    """Ctrl+L, then read.

    **ratatui only sends the cells that changed**, so a fold that flips one marker emits a lone
    glyph with no context and the replay of a partial frame is ambiguous. Asking for a full
    repaint makes the screen say what it actually shows.
    """
    os.write(fd, CTRL_L)
    drain(fd, buf, seconds)


def show(title, buf):
    print(f"\n╭─ {title} " + "─" * (COLS - len(title) - 4))
    print(screen(buf))
    print("╰" + "─" * (COLS - 1))


def main():
    index = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    if not os.path.exists(BIN):
        print(f"{BIN} is missing — build it first.", file=sys.stderr)
        return 1

    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-history.log",
    )
    primary, replica = pty.openpty()
    fcntl.ioctl(replica, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    proc = subprocess.Popen(
        [BIN], stdin=replica, stdout=replica, stderr=replica, env=env, close_fds=True
    )
    os.close(replica)

    buf = []
    try:
        # **Wait for the connection, not the first frame.** The TUI comes up before the node
        # attaches, and the lists are empty until it has — pressing ← at that point opens a list
        # with nothing in it, and every screen after is of a session that was never opened.
        if not wait_for(primary, "Connected", buf, 90, "connection"):
            show("what was drawn", buf)
            return 1
        drain(primary, buf, 3)

        # ← opens the **project** list, and picking one opens that project's sessions. Both lists
        # lead with a "+ New …" row, so a plain Enter would create instead of open — step past it.
        os.write(primary, LEFT)
        drain(primary, buf, 9)
        show("project list", buf)

        buf.clear()
        os.write(primary, DOWN)
        os.write(primary, ENTER)
        drain(primary, buf, 9)
        show("session list", buf)

        buf.clear()
        for _ in range(index + 1):
            os.write(primary, DOWN)
            drain(primary, buf, 0.4)
        os.write(primary, ENTER)
        # History replay walks the whole session, so give it room.
        drain(primary, buf, 18)
        repaint(primary, buf)
        show("history, as it opened", buf)

        # Ctrl+O toggles the last card's head — the whole stretch of working, in one press.
        # Chips and tool rows are click-only, so nothing else moves here.
        os.write(primary, CTRL_O)
        drain(primary, buf, 2)
        repaint(primary, buf)
        show("after Ctrl+O", buf)

        os.write(primary, CTRL_O)
        drain(primary, buf, 2)
        repaint(primary, buf)
        show("after Ctrl+O again", buf)
    finally:
        os.write(primary, CTRL_C)
        time.sleep(0.5)
        os.write(primary, CTRL_C)
        time.sleep(1.0)
        if proc.poll() is None:
            proc.kill()
        os.close(primary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
