#!/usr/bin/env python3
"""실제 pty에서 **슬래시 명령**을 한 바퀴 돌린다.

서버에 붙어야 화면이 뜨므로(`on_connect` 안에서 TUI가 시작된다) 라이브가 필요하다.
다만 **턴은 돌리지 않는다** — 크레딧을 쓰지 않고, 명령이 서버로 안 간다는 것 자체가
여기서 보려는 것이다.

```bash
python3 scripts/command_smoke.py
```
"""

import fcntl
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time

BIN = "target/debug/zyris-code"
SCRATCH = "/tmp/zyris-code-command"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")


# **The settings this run must put back.**
#
# These scripts drive the real app against the real `~/.config/zyris-code` — the credentials live
# there, so `HOME` cannot be moved without the app losing its enrolment and never coming up. That
# means a stray Enter inside the `/config` form writes the person's actual settings, and it really
# happened: a run saved `default_mode: Job`, so the next launch opened in 작업 mode, the bottom bar
# no longer said 일반, and the first-frame check failed on a perfectly healthy app.
SETTINGS = [
    os.path.expanduser("~/.config/zyris-code/config.json"),
    os.path.expanduser("~/.config/zyris-code/lang"),
]


def keep_settings():
    """Reads the settings aside. Returns what `restore_settings` needs."""
    kept = {}
    for path in SETTINGS:
        try:
            with open(path, "rb") as f:
                kept[path] = f.read()
        except OSError:
            kept[path] = None
    return kept


def restore_settings(kept):
    """Puts them back exactly, including "there was no file"."""
    for path, body in kept.items():
        try:
            if body is None:
                os.path.exists(path) and os.remove(path)
            else:
                with open(path, "wb") as f:
                    f.write(body)
        except OSError:
            pass



def read_until(fd, needle, buf, deadline, label):
    """누적 버퍼에서 needle이 나올 때까지 읽는다. 읽기 전에 지금 버퍼부터 본다.

    **스냅숏 하나만 뒤지면 안 된다.** 머리말이 보이는 순간 돌아오므로 그 아래 줄들은
    아직 안 와 있다 — `search_smoke.py`가 이미 같은 자리에서 걸렸다.
    """
    while True:
        if needle in ANSI.sub("", "".join(buf)):
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
    print(f"  ✗ {label}: '{needle}'를 못 봤다")
    return False


def send(fd, text, buf=None):
    """Types, then settles **while keeping the pty drained.**

    **Never sleep without reading.** The buffer fills, the app blocks part-way through a draw,
    and keys already sent sit unprocessed — which looks exactly like "the command did nothing".
    `plugin_smoke.py` lost six checks to this and read as a broken `/plugin add`.
    """
    os.write(fd, text.encode())
    end = time.time() + 0.4
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                chunk = os.read(fd, 262144)
            except OSError:
                return
            # **Keep what was read.** Draining must not swallow the very output the checks
            # then look for — discarding it here cost a check that had been passing.
            if chunk and buf is not None:
                buf.append(chunk.decode("utf-8", "replace"))


def main():
    if not os.path.exists(BIN):
        print(f"{BIN}이 없다. 먼저 `cargo build -j2` 할 것.")
        return 1

    shutil.rmtree(SCRATCH, ignore_errors=True)
    os.makedirs(SCRATCH)
    print(f"작업 디렉터리: {SCRATCH}\n")

    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-command.log",
    )
    primary, replica = pty.openpty()
    # **The window size must be set.** At 0×0 ratatui emits nothing.
    fcntl.ioctl(replica, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    proc = subprocess.Popen(
        [os.path.abspath(BIN)],
        stdin=replica,
        stdout=replica,
        stderr=replica,
        env=env,
        cwd=SCRATCH,
        close_fds=True,
    )
    os.close(replica)

    buf = []
    ok = True
    checks = 0
    total = 9

    def check(passed, label):
        nonlocal ok, checks
        if passed:
            print(f"  ✓ {label}")
            checks += 1
        else:
            print(f"  ✗ {label}")
            ok = False

    kept = keep_settings()
    try:
        if not read_until(primary, "일반", buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        # 1. Pressing `/` shows the list. **If it doesn't show, nobody will use it.**
        send(primary, "/", buf)
        check(read_until(primary, "/mode", buf, time.time() + 5, "명령 목록"), "`/`에 목록이 뜬다")
        # **Wait, then read.** The check above stops as soon as `/mode` appears, so the lines below
        # it haven't arrived yet — a snapshot-only search would say they're missing.
        check(
            read_until(primary, "/agent", buf, time.time() + 5, "/agent 줄"),
            "`/agent`이 목록에 있다",
        )

        # 2. Typing narrows it down.
        #
        # **Read the redraw, not one snapshot.** ratatui only puts changed cells on the wire, so
        # this reads until `/cwd` shows up in the bytes that arrived after the keystrokes — the
        # box shrinks to one row, so that row is always redrawn. `/mode` must not be among them:
        # its row is now blank, and a blank is written as spaces.
        #
        # **Ctrl+L cannot be used to force a full frame here.** With the list up, `on_key`'s
        # picker branch takes every character as input, so Ctrl+L types an `l` and narrows the
        # list to nothing instead of repainting.
        buf.clear()
        send(primary, "cw", buf)
        narrowed = read_until(primary, "/cwd", buf, time.time() + 5, "좁혀진 목록")
        shown = ANSI.sub("", "".join(buf))
        check(narrowed and "/mode" not in shown, "치는 대로 좁혀진다")

        # 3. Run it. **It answers here instead of going to the server.**
        buf.clear()
        send(primary, "\r", buf)
        check(
            read_until(primary, SCRATCH, buf, time.time() + 8, "/cwd 결과"),
            "`/cwd`가 작업 디렉터리를 답한다",
        )

        # 4. Change the mode.
        buf.clear()
        send(primary, "/mode 계획", buf)
        send(primary, "\r", buf)
        check(
            read_until(primary, "계획", buf, time.time() + 8, "/mode 결과"),
            "`/mode 계획`이 모드를 바꾼다",
        )

        # 5. `/mcp` must say something even with nothing attached — silence reads as a bug.
        buf.clear()
        send(primary, "/mcp", buf)
        send(primary, "\r", buf)
        check(
            read_until(primary, "MCP", buf, time.time() + 8, "/mcp 결과"),
            "`/mcp`가 답한다",
        )
        # **Close it before typing anything else.** The panel is modal — it swallows every key
        # but Esc — so a command typed with one still up goes nowhere at all. This script was
        # written before the panels existed and silently fed `/config` to the MCP panel.
        send(primary, "\x1b", buf)
        time.sleep(0.4)

        # 6. `/config` opens the settings form — one row per setting, its value in brackets.
        buf.clear()
        send(primary, "/config", buf)
        send(primary, "\r", buf)
        check(
            read_until(primary, "다른 디렉토리 접근", buf, time.time() + 8, "/config 결과"),
            "`/config`가 설정 창을 연다",
        )
        # **Esc, not Enter.** Enter would save, and this script runs against the real
        # `~/.config/zyris-code` — a smoke test must not rewrite the user's settings.
        send(primary, "\x1b", buf)
        time.sleep(0.4)

        # 7. **`/quit` must really exit.** Only observable here — a shell assertion
        # can't tell whether the process is alive.
        buf.clear()
        send(primary, "/quit", buf)
        send(primary, "\r", buf)
        # **Keep draining while waiting.** Nothing reads the pty in a bare sleep loop, so the
        # buffer fills and the app blocks mid-draw — it then never gets to run `/quit` at all
        # and this reads as "quitting is broken". It is the same trap the approval window used
        # to hide, and it only shows once enough output has piled up ahead of it.
        left = False
        for _ in range(60):
            if proc.poll() is not None:
                left = True
                break
            r, _, _ = select.select([primary], [], [], 0.1)
            if r:
                try:
                    os.read(primary, 65536)
                except OSError:
                    left = proc.poll() is not None
                    break
        check(left, "`/quit`가 프로세스를 끝낸다")

    finally:
        restore_settings(kept)

    return finish(proc, primary, checks, total, ok)


def finish(proc, primary, checks, total, ok):
    if proc.poll() is None:
        proc.send_signal(signal.SIGKILL)
    os.close(primary)
    print(f"\n{checks}/{total} 통과")
    return 0 if ok and checks == total else 1


if __name__ == "__main__":
    sys.exit(main())
