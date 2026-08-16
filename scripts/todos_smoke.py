#!/usr/bin/env python3
"""Runs one real turn and checks the session's plan reaches the screen.

The todo list is rebuilt from the agent's todo tool calls (`todos.rs`), so nothing here can be
seen without a turn actually running — this is the one script that proves the wiring end to end.

**It spends credits.** One turn, and the turn is asked to do nothing but write itself a plan.

The verdict is the screen, and that is safe here: the count `(n/m)` and the task rows are drawn
by this app from tool results, so they cannot come from the prompt the way a tool name can
(the trap `search_smoke.py` fell into). The prompt below deliberately never says a number.
"""

import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import termios
import time

BIN = "target/debug/zyris-code"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")
# **Nothing here may name what the checks look for.** The prompt is echoed onto the screen, so a
# word that appears in both proves nothing — `search_smoke.py` once passed on its own prompt.
ASK = (
    "Use your todo tool only. Add exactly three tasks, then mark the first one completed. "
    "Do not read or write any file. Answer with one word: done."
)
DUMP = "/tmp/zyris-code-todos-screen.txt"


def screen(buf):
    return ANSI.sub("", "".join(buf))


def send(fd, text, buf, settle=0.6):
    """Types, then settles **while keeping the pty drained.**

    **Never sleep without reading.** The buffer fills, the app blocks part-way through a draw, and
    the keys already sent sit unprocessed — which looks exactly like "Enter did nothing". This
    script lost a whole turn to it: the prompt stayed in the input box and was never sent.
    """
    os.write(fd, text.encode())
    end = time.time() + settle
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if not r:
            continue
        try:
            chunk = os.read(fd, 262144)
        except OSError:
            return
        # **Keep what was read.** Draining must not swallow the output the checks then look for.
        if chunk:
            buf.append(chunk.decode("utf-8", "replace"))


def read_until(fd, needles, buf, deadline, label):
    """Waits until one of `needles` shows on screen. Drains the pty meanwhile.

    **The buffer is checked before reading.** Once the app has drawn and gone quiet no new bytes
    arrive, and looking only after a read would never find what is already there.
    """
    if isinstance(needles, str):
        needles = [needles]
    while True:
        if any(n in screen(buf) for n in needles):
            return True
        if time.time() >= deadline:
            break
        r, _, _ = select.select([fd], [], [], 0.5)
        if not r:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf.append(chunk.decode("utf-8", "replace"))
    print(f"  ✗ {label}: {' or '.join(needles)} never showed")
    return False


def main():
    if not os.path.exists(BIN):
        print(f"{BIN} is missing. Build it first.")
        return 1

    env = dict(
        os.environ,
        ZYRIS_CODE_LANG="ko",
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-todos.log",
    )
    primary, replica = pty.openpty()
    fcntl.ioctl(replica, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    proc = subprocess.Popen(
        [os.path.abspath(BIN)], stdin=replica, stdout=replica, stderr=replica, env=env,
        close_fds=True,
    )
    os.close(replica)

    buf, checks, ok = [], 0, True
    try:
        if not read_until(primary, ["일반", "계획", "작업", "일"], buf, time.time() + 40, "first frame"):
            return 1
        print("  ✓ it came up")
        checks += 1

        # The server snapshots this node's capabilities within 500ms of the handshake, so a turn
        # started the instant it connects sees an empty tool list.
        send(primary, "", buf, settle=4.0)
        send(primary, ASK, buf, settle=1.5)
        send(primary, "\r", buf, settle=1.0)

        # **The count is the verdict.** It is drawn from tool results, never from the prompt.
        if read_until(primary, ["(1/3)"], buf, time.time() + 300, "the plan count"):
            print("  ✓ the activity line counts the plan (1/3)")
            checks += 1
        else:
            ok = False

        # Ctrl+T unfolds it. **Ctrl+L first** — ratatui only emits changed cells, so without a
        # full repaint the rows may arrive as fragments that no search can read.
        send(primary, "\x14", buf, settle=0.8)
        send(primary, "\x0c", buf, settle=1.0)
        # **The numbering is the widget's own** (`widgets/todos.rs`), so unlike the task words it
        # cannot have come from the prompt.
        if read_until(primary, ["1. ", "2. ", "3. "], buf, time.time() + 15, "the unfolded plan"):
            print("  ✓ Ctrl+T unfolds the tasks")
            checks += 1
        else:
            ok = False

        # And the count survives the turn ending — a plan left half done is what a person wants
        # to look at once the agent has stopped.
        send(primary, "", buf, settle=2.0)
        if "(1/3)" in screen(buf):
            print("  ✓ the count stays after the turn ends")
            checks += 1
        else:
            ok = False
    finally:
        with open(DUMP, "w") as f:
            f.write(screen(buf))
        print(f"  (screen dumped to {DUMP})")
        if proc.poll() is None:
            proc.send_signal(signal.SIGKILL)
        os.close(primary)

    print(f"\n{checks}/4 passed")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
