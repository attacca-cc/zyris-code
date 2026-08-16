#!/usr/bin/env python3
"""**작업 디렉터리 밖은 설정이 가른다**를 진짜 턴으로 확인한다. 크레딧을 쓴다.

승인 창은 없어졌다(2026-08-07 사용자 결정). 밖을 어떻게 대할지는 `/config`의 `dir`
설정이 정한다 — `allow`는 통과시키고, `deny`(기본)는 거부한다.

판정은 디스크로 한다. 밖에 심어 둔 파일을 안쪽으로 베껴 오게 시켜서, `allow`에서는
생기고 `deny`에서는 안 생기는지 본다 — `deny`인데 생겼으면 정책이 아무것도 막지 않은
것이다.

```bash
python3 scripts/sandbox_smoke.py
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
# Where the app runs. Only this and what's below it must be touchable.
INSIDE = "/tmp/zyris-code-sandbox/안"
# Outside of it. `deny` must make it unreadable.
OUTSIDE = "/tmp/zyris-code-sandbox/밖"
SECRET = "울타리너머의비밀"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")

# Step 1: touch only the inside. It must just run.
STEP1 = (
    "이름이 '__code_edit__write'로 끝나는 도구로 path='안것.txt'에 '안녕'이라고 써라. "
    "설명은 하지 마라."
)
# Step 2: go outside with `allow` on — the copy must land.
STEP2 = (
    f"이름이 '__file_io__read'로 끝나는 도구로 path='{OUTSIDE}/비밀.txt'를 읽고, "
    "그 안에 적힌 낱말을 이름이 '__code_edit__write'로 끝나는 도구로 path='밖것.txt'에 써라."
)
# Step 3: go outside with `deny` on — the copy must not land.
STEP3 = (
    f"이름이 '__file_io__read'로 끝나는 도구로 path='{OUTSIDE}/비밀.txt'를 읽고, "
    "그 안에 적힌 낱말을 이름이 '__code_edit__write'로 끝나는 도구로 path='밖것2.txt'에 써라."
)
# The refusal the gate sends. The agent usually relays it verbatim.
REFUSAL = "작업 디렉터리 밖"


def read_until(fd, needles, buf, deadline, label):
    """화면에서 needles 중 하나가 보일 때까지 기다린다. 하나도 안 보이면 False."""
    if isinstance(needles, str):
        needles = [needles]
    while True:
        text = ANSI.sub("", "".join(buf))
        if any(n in text for n in needles):
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
    print(f"  ✗ {label}: {'·'.join(needles)}를 못 봤다")
    return False


def wait_for_file(fd, buf, at, needle, secs):
    """파일에 needle이 나타날 때까지 기다린다. **그동안 pty도 계속 빨아들인다** —
    안 읽으면 버퍼가 차서 앱이 그리다 멈춘다."""
    deadline = time.time() + secs
    while time.time() < deadline:
        time.sleep(0.5)
        r, _, _ = select.select([fd], [], [], 0)
        if r:
            try:
                buf.append(os.read(fd, 65536).decode("utf-8", "replace"))
            except OSError:
                pass
        if os.path.exists(at) and needle in open(at).read():
            return True
    return False


def type_line(primary, text, pause=0.6):
    """입력란에 한 줄을 치고 Enter로 보낸다. 슬래시 명령은 로컬이라 즉시 처리된다."""
    os.write(primary, text.encode())
    time.sleep(pause)
    os.write(primary, b"\r")


def main():
    if not os.path.exists(BIN):
        print(f"{BIN}이 없다. 먼저 `cargo build -j2` 할 것.")
        return 1

    shutil.rmtree("/tmp/zyris-code-sandbox", ignore_errors=True)
    os.makedirs(INSIDE)
    os.makedirs(OUTSIDE)
    with open(os.path.join(OUTSIDE, "비밀.txt"), "w") as f:
        f.write(f"{SECRET}\n")
    print(f"안: {INSIDE}\n밖: {OUTSIDE}\n")

    env = dict(
        os.environ,
        ZYRIS_CODE_LANG="ko",
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-sandbox.log",
    )
    primary, replica = pty.openpty()
    fcntl.ioctl(replica, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    proc = subprocess.Popen(
        [os.path.abspath(BIN)],
        stdin=replica,
        stdout=replica,
        stderr=replica,
        env=env,
        cwd=INSIDE,
        close_fds=True,
    )
    os.close(replica)

    buf = []
    ok = True
    checks = 0
    total = 5

    try:
        if not read_until(primary, ("일반", "계획", "작업", "일"), buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        # ── 안쪽 일은 그냥 된다 ──
        time.sleep(3)
        type_line(primary, STEP1)

        if wait_for_file(primary, buf, os.path.join(INSIDE, "안것.txt"), "안녕", 240):
            print("  ✓ 안쪽 일은 그냥 된다")
            checks += 1
        else:
            print("  ✗ 안쪽 일이 안 됐다")
            return finish(proc, primary, checks, total, False)

        if REFUSAL not in ANSI.sub("", "".join(buf)):
            print("  ✓ 안쪽 일에는 아무것도 안 묻는다")
            checks += 1
        else:
            print("  ✗ 안쪽인데 거부 메시지가 떴다")
            ok = False

        # ── `allow`면 밖도 통한다 ──
        buf.clear()
        type_line(primary, "/config dir allow")
        if not read_until(primary, "허용", buf, time.time() + 20, "허용 확인"):
            return finish(proc, primary, checks, total, False)
        buf.clear()
        time.sleep(2)
        type_line(primary, STEP2)

        if wait_for_file(primary, buf, os.path.join(INSIDE, "밖것.txt"), SECRET, 240):
            print("  ✓ allow면 밖 읽기가 통한다")
            checks += 1
        else:
            print("  ✗ allow인데 밖 읽기가 안 됐다")
            return finish(proc, primary, checks, total, False)

        # ── `deny`면 밖이 막힌다 ──
        buf.clear()
        type_line(primary, "/config dir deny")
        if not read_until(primary, "거부", buf, time.time() + 20, "거부 확인"):
            return finish(proc, primary, checks, total, False)
        buf.clear()
        time.sleep(2)
        type_line(primary, STEP3)

        if not read_until(primary, [REFUSAL, "만질 수 없습니다", "못"], buf, time.time() + 240,
                          "거부 메시지"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ deny면 밖을 거부한다")
        checks += 1

        # **거부된 읽기가 아무것도 만들지 않는다.** 생겼으면 정책이 체다.
        out2 = os.path.join(INSIDE, "밖것2.txt")
        leaked = os.path.exists(out2) and SECRET in open(out2).read()
        time.sleep(3)
        leaked = leaked or (os.path.exists(out2) and SECRET in open(out2).read())

        if leaked:
            print("  ✗ 거부했는데도 밖의 내용이 새어 나왔다")
            ok = False
        else:
            print("  ✓ deny에서는 아무것도 새어 나오지 않는다")
            checks += 1

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
