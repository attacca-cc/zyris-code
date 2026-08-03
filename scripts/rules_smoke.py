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
# 어디에도 없을 값. 다른 데서 새어 들어오면 검사가 거짓 양성이 된다.
SECRET = "무지개돌고래"
OUT = "암호.txt"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")
# **파일을 읽지 말라고 못 박는다.** 안 그러면 `file_io.read`로 CLAUDE.md를 읽어 와서
# 맞히고, 그러면 preamble이 실렸는지는 아무것도 증명하지 못한다.
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
        if not until(primary, "기본", buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        time.sleep(3)
        # `/rules`가 무엇을 실었다고 말하는지 먼저 본다. **Enter 한 번에 돌아야 한다** —
        # 목록이 떠 있다고 Enter가 "고르기"로만 먹히면 아무 일도 안 일어난다.
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

        # **판정은 디스크로 한다.** 에이전트가 뭐라고 말했는지는 보지 않는다.
        at = os.path.join(SCRATCH, OUT)
        got = ""
        deadline = time.time() + 240
        while time.time() < deadline:
            time.sleep(0.5)
            # **화면도 계속 빨아들인다.** 안 읽으면 pty 버퍼가 차서 앱이 그리다 멈춘다 —
            # 승인 창을 기다리던 코드가 예전엔 그 일을 겸하고 있었다.
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
