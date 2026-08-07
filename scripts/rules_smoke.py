#!/usr/bin/env python3
"""`CLAUDE.md`가 세션에 실리는지 **부작용으로** 본다. 크레딧을 쓴다.

preamble은 화면에 안 보이는 자리다. 조용히 안 실려도 티가 안 나므로, 리포에 그 파일에만
있는 값을 심어 두고 **에이전트가 그것을 써낼 수 있는지**로 판정한다 — 읽지 못했으면
쓸 수 없는 값이라 판정이 명확하다.

```bash
python3 scripts/rules_smoke.py
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
SCRATCH = "/tmp/zyris-code-rules"
# A value that exists nowhere. If it leaks in from somewhere else, the check becomes a false positive.
SECRET = "무지개돌고래"
OUT = "암호.txt"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")
# **It pins down that no file may be read.** Otherwise it would read CLAUDE.md with `file_io.read` and
# get it right — and then nothing proves the preamble was loaded.
ASK = (
    "이 프로젝트의 지침에 적힌 암호를 이름이 '__code_edit__write'로 끝나는 도구로 "
    f"path='{OUT}'에 그대로 써라. 어떤 파일도 읽지 말고 이미 아는 것으로 답하라."
)


def until(fd, needle, buf, deadline, label):
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


def main():
    if not os.path.exists(BIN):
        print(f"{BIN}이 없다. 먼저 `cargo build -j2` 할 것.")
        return 1

    shutil.rmtree(SCRATCH, ignore_errors=True)
    os.makedirs(SCRATCH)
    with open(os.path.join(SCRATCH, "CLAUDE.md"), "w") as f:
        f.write(f"# 이 프로젝트\n\n## 암호\n\n이 프로젝트의 암호는 `{SECRET}`다.\n")
    print(f"작업 디렉터리: {SCRATCH}\n")

    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-rules.log",
    )
    primary, replica = pty.openpty()
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
    total = 3

    try:
        if not until(primary, "일반", buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        time.sleep(3)
        # First see what `/rules` says it loaded. **It must run in one Enter** —
        # if Enter only acts as "select" because a list is showing, nothing happens.
        os.write(primary, b"/rules\r")
        if until(primary, "CLAUDE.md", buf, time.time() + 10, "/rules 결과"):
            print("  ✓ `/rules`가 CLAUDE.md를 실었다고 말한다")
            checks += 1
        else:
            ok = False

        time.sleep(1)
        os.write(primary, ASK.encode())
        time.sleep(0.5)
        os.write(primary, b"\r")

        # **The verdict comes from the disk.** What the agent says is not examined.
        at = os.path.join(SCRATCH, OUT)
        got = ""
        deadline = time.time() + 240
        while time.time() < deadline:
            time.sleep(0.5)
            # **Keep draining the screen too.** If it isn't read, the pty buffer fills and the app stalls mid-draw —
            # the code that used to wait for the approval window used to do that job.
            r, _, _ = select.select([primary], [], [], 0)
            if r:
                buf.append(os.read(primary, 65536).decode("utf-8", "replace"))
            if os.path.exists(at):
                got = open(at).read()
                if got.strip():
                    break
        if SECRET in got:
            print("  ✓ 에이전트가 CLAUDE.md의 내용을 알고 있다")
            checks += 1
        else:
            print(f"  ✗ 지침이 세션에 안 실렸다: {got!r}")
            ok = False

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
