#!/usr/bin/env python3
"""실제 PTY에서 앱을 띄워 화면이 그려지고 깨끗하게 빠져나오는지 본다.

이 머신에는 tmux/screen/expect가 없다(설치 권한도 없다). 그래서 파이썬 표준 `pty`로
직접 붙인다.

여기서 확인하는 것은 **셀 단언이 못 보는 것**뿐이다 — 진짜 터미널을 잡았다가 되돌리는지,
좁은 폭에서 패닉하지 않는지. 대화 동작은 사람이 본다.

주의: ratatui는 **바뀐 셀만** 내보낸다. 중간 글자를 기다리면 안 되고, 누적 버퍼에서
찾아야 한다.

**서버에 붙어야 돌아간다.** TUI는 `on_connect` 안에서 뜨므로 연결이 안 되면 한 프레임도
안 그려지고 검사가 통째로 무너진다. 그때 3·4번이 통과한 것처럼 보이는데 그건 앱이 아니라
pty의 줄 편집기가 친 글자를 되비춘 것이다 — **2/8로 끝나면 먼저 로그의 connect failed부터
볼 것.**
"""

import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

BIN = "target/debug/zyris-code"
DEADLINE = 40.0
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")


def read_until(fd, needle, buf, deadline, label):
    """누적 버퍼에서 needle이 나올 때까지 읽는다.

    **읽기 전에 먼저 지금 버퍼를 본다.** 앱이 다 그리고 조용해지면 새 바이트가 오지
    않는데, 읽은 뒤에만 검사하면 이미 화면에 있는 것도 영영 못 찾는다 — 앞선 검사가
    받아 둔 출력에 답이 들어 있는 경우가 그렇다. 실제로 이것 때문에 멀쩡한 앱이
    5개 항목에서 한꺼번에 실패했다.
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


def main():
    if not os.path.exists(BIN):
        print(f"{BIN}이 없다. 먼저 `cargo build -j2` 할 것.")
        return 1

    # Keep self-healing short so it definitely runs at least once before check 4.
    #
    # Also cut the wire deadline to an extreme. This value is only used where approval is awaited,
    # but if the small value bites somewhere in the startup path, the checks below collapse entirely — that is what we watch for.
    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-smoke.log",
        ZYRIS_CODE_HEAL_MS="500",
        ZYRIS_CODE_WIRE_DEADLINE_SECS="1",
    )
    primary, replica = pty.openpty()
    # **The window size must be set.** The pty `pty.openpty()` creates is 0×0, and ratatui sees
    # no room to draw and emits nothing — a perfect setup for misdiagnosing a healthy app as
    # "no screen". It actually caught us once.
    fcntl.ioctl(replica, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    proc = subprocess.Popen(
        [BIN], stdin=replica, stdout=replica, stderr=replica, env=env, close_fds=True
    )
    os.close(replica)

    buf = []
    deadline = time.time() + DEADLINE
    ok = True
    checks = 0

    try:
        # 1. The first frame appeared = it grabbed the alternate screen and started drawing.
        #
        # We used to look for the header ("zyris-code") but removed that line. The mode in the
        # bottom bar is always drawn whether connected or not, so it is the better target — the
        # status line text swings between "connecting…" and "idle" depending on the connection.
        #
        # **It is the mode label, so it moves when the wording does.** This looked for "기본"
        # long after `lang::mode_normal` had become "일반", and the check had been failing on a
        # perfectly healthy app ever since.
        if read_until(primary, "일반", buf, deadline, "첫 프레임"):
            print("  ✓ 첫 프레임이 그려진다")
            checks += 1
        else:
            ok = False

        # 2. The input field prompt is there.
        #
        # **It has to be read by waiting.** Check 1 stops as soon as it sees the status line, so
        # the buffer at that point does not yet hold the whole first frame — searching only what was already read says it is not there.
        if read_until(primary, "> ", buf, time.time() + 5, "입력란"):
            print("  ✓ 입력란이 있다")
            checks += 1
        else:
            ok = False

        # 3. Typing Korean does not crash it and it shows up as-is.
        os.write(primary, "안녕".encode())
        if read_until(primary, "안녕", buf, time.time() + 5, "한글 입력"):
            print("  ✓ 한글 입력이 보인다")
            checks += 1
        else:
            ok = False

        # 4. **Still alive after self-healing has run once.**
        #
        # This pty does not answer cursor position queries (DSR) — like a remote terminal that is
        # slow or silent. When we used `Terminal::clear()` before, the app froze entirely here.
        # With the self-healing interval kept short so it definitely runs once, check whether keys still work.
        time.sleep(1.0)
        os.write(primary, "하세요".encode())
        if read_until(primary, "하세요", buf, time.time() + 5, "치유 뒤 입력"):
            print("  ✓ 다시 그린 뒤에도 키가 먹는다")
            checks += 1
        else:
            ok = False

        # 5. **The sidebar states the tool's working directory.**
        #
        # While the agent is touching this computer, if where it is working is not on screen, the
        # relative path shown on the approval screen cannot be read. The cell assertions look
        # too, but here the point is whether it survives **real terminal width calculation**.
        if read_until(primary, "zyris-code", buf, time.time() + 5, "작업 디렉터리"):
            print("  ✓ 사이드바가 작업 디렉터리를 보여준다")
            checks += 1
        else:
            ok = False

        # 6. One Ctrl+C does not end it; a notice appears.
        os.write(primary, b"\x03")
        if read_until(primary, "한 번 더 Ctrl+C", buf, time.time() + 5, "종료 예고"):
            print("  ✓ Ctrl+C 한 번은 예고만 한다")
            checks += 1
        else:
            ok = False
        time.sleep(0.3)
        if proc.poll() is not None:
            print("  ✗ 한 번에 꺼져 버렸다")
            ok = False

        # 7. Pressing again within 1.5 seconds ends it.
        os.write(primary, b"\x03")
        for _ in range(40):
            if proc.poll() is not None:
                break
            time.sleep(0.1)
        if proc.poll() is not None:
            print("  ✓ 두 번째 Ctrl+C로 끝난다")
            checks += 1
        else:
            print("  ✗ 두 번 눌러도 안 끝난다")
            ok = False

        # 8. The alternate screen was restored. Without it, the shell is left broken.
        raw = "".join(buf)
        # Also read the bytes left behind on exit.
        r, _, _ = select.select([primary], [], [], 1.0)
        if r:
            try:
                raw += os.read(primary, 65536).decode("utf-8", "replace")
            except OSError:
                pass
        if "\x1b[?1049l" in raw:
            print("  ✓ 대체 화면을 되돌렸다")
            checks += 1
        else:
            print("  ✗ 대체 화면 복구 시퀀스가 안 보인다")
            ok = False

    finally:
        if proc.poll() is None:
            proc.send_signal(signal.SIGKILL)
        os.close(primary)

    print(f"\n{checks}/8 통과")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
