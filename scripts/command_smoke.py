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


def send(fd, text):
    os.write(fd, text.encode())
    time.sleep(0.4)


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

    try:
        if not read_until(primary, "기본", buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        # 1. Pressing `/` shows the list. **If it doesn't show, nobody will use it.**
        send(primary, "/")
        check(read_until(primary, "/mode", buf, time.time() + 5, "명령 목록"), "`/`에 목록이 뜬다")
        # **Wait, then read.** The check above stops as soon as `/mode` appears, so the lines below
        # it haven't arrived yet — a snapshot-only search would say they're missing.
        check(
            read_until(primary, "/agent", buf, time.time() + 5, "/agent 줄"),
            "`/agent`이 목록에 있다",
        )

        # 2. Typing narrows it down.
        buf.clear()
        send(primary, "cw")
        time.sleep(0.6)
        r, _, _ = select.select([primary], [], [], 1.0)
        if r:
            buf.append(os.read(primary, 262144).decode("utf-8", "replace"))
        shown = ANSI.sub("", "".join(buf))
        check("/cwd" in shown and "/mode" not in shown, "치는 대로 좁혀진다")

        # 3. Run it. **It answers here instead of going to the server.**
        buf.clear()
        send(primary, "\r")
        check(
            read_until(primary, SCRATCH, buf, time.time() + 8, "/cwd 결과"),
            "`/cwd`가 작업 디렉터리를 답한다",
        )

        # 4. Change the mode.
        buf.clear()
        send(primary, "/mode 계획")
        send(primary, "\r")
        check(
            read_until(primary, "계획", buf, time.time() + 8, "/mode 결과"),
            "`/mode 계획`이 모드를 바꾼다",
        )

        # 5. `/mcp` must say something even with nothing attached — silence reads as a bug.
        buf.clear()
        send(primary, "/mcp")
        send(primary, "\r")
        check(
            read_until(primary, "MCP", buf, time.time() + 8, "/mcp 결과"),
            "`/mcp`가 답한다",
        )

        # 6. `/config` opens the settings panel — the values are marked there.
        buf.clear()
        send(primary, "/config")
        send(primary, "\r")
        check(
            read_until(primary, "다른 디렉토리 접근", buf, time.time() + 8, "/config 결과"),
            "`/config`가 설정 창을 연다",
        )

        # 7. **`/quit` must really exit.** Only observable here — a shell assertion
        # can't tell whether the process is alive.
        buf.clear()
        send(primary, "/quit")
        send(primary, "\r")
        left = False
        for _ in range(60):
            if proc.poll() is not None:
                left = True
                break
            time.sleep(0.1)
        check(left, "`/quit`가 프로세스를 끝낸다")

    finally:
        pass

    return finish(proc, primary, checks, total, ok)


def finish(proc, primary, checks, total, ok):
    if proc.poll() is None:
        proc.send_signal(signal.SIGKILL)
    os.close(primary)
    print(f"\n{checks}/{total} 통과")
    return 0 if ok and checks == total else 1


if __name__ == "__main__":
    sys.exit(main())
