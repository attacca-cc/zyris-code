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

    # 자가 치유를 짧게 잡아 검사 4번 전에 반드시 한 번은 돌게 한다.
    #
    # 와이어 마감도 극단으로 줄여 둔다. 이 값은 승인을 기다리는 자리에서만 쓰이지만,
    # 작은 값이 시작 경로 어딘가를 물게 하면 아래 검사가 통째로 무너진다 — 그걸 본다.
    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-smoke.log",
        ZYRIS_CODE_HEAL_MS="500",
        ZYRIS_CODE_WIRE_DEADLINE_SECS="1",
    )
    primary, replica = pty.openpty()
    # **창 크기를 반드시 정한다.** `pty.openpty()`가 만든 pty는 0×0이고, ratatui는 그릴
    # 자리가 없다고 보아 아무것도 내보내지 않는다 — 앱이 멀쩡한데 "화면이 안 뜬다"로
    # 오진하기 딱 좋다. 실제로 한 번 걸렸다.
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
        # 1. 첫 프레임이 나왔다 = 대체 화면을 잡고 그리기 시작했다.
        #
        # 예전에는 머리글("zyris-code")을 봤는데 그 줄을 없앴다. 하단 바의 모드는
        # 붙었든 안 붙었든 늘 그려지므로 이쪽이 낫다 — 상태 줄 글자는 연결 여부에
        # 따라 "연결 중…"과 "쉬는 중" 사이를 오간다.
        if read_until(primary, "기본", buf, deadline, "첫 프레임"):
            print("  ✓ 첫 프레임이 그려진다")
            checks += 1
        else:
            ok = False

        # 2. 입력란 프롬프트가 있다.
        #
        # **기다려서 읽어야 한다.** 1번 검사는 상태 줄을 보자마자 멈추므로 그 시점의
        # 버퍼에는 첫 프레임이 아직 다 안 들어와 있다 — 이미 읽은 것만 뒤지면 없다고 나온다.
        if read_until(primary, "> ", buf, time.time() + 5, "입력란"):
            print("  ✓ 입력란이 있다")
            checks += 1
        else:
            ok = False

        # 3. 한글을 쳐도 죽지 않고 그대로 보인다.
        os.write(primary, "안녕".encode())
        if read_until(primary, "안녕", buf, time.time() + 5, "한글 입력"):
            print("  ✓ 한글 입력이 보인다")
            checks += 1
        else:
            ok = False

        # 4. **자가 치유가 한 번 돈 뒤에도 살아 있다.**
        #
        # 이 pty는 커서 위치를 물어봐도(DSR) 대답하지 않는다 — 답이 늦거나 없는 원격
        # 터미널과 같다. 예전에 `Terminal::clear()`를 쓸 때는 여기서 앱이 통째로 멈췄다.
        # 자가 치유 간격을 짧게 잡아 반드시 한 번은 돌게 한 뒤, 키가 아직 먹는지 본다.
        time.sleep(1.0)
        os.write(primary, "하세요".encode())
        if read_until(primary, "하세요", buf, time.time() + 5, "치유 뒤 입력"):
            print("  ✓ 다시 그린 뒤에도 키가 먹는다")
            checks += 1
        else:
            ok = False

        # 5. **사이드바가 도구의 작업 디렉터리를 말한다.**
        #
        # 에이전트가 이 컴퓨터를 만지는 지금, 어느 자리에서 만지는지가 화면에 없으면
        # 승인 화면에 뜬 상대경로를 읽을 수 없다. 셀 단언도 보는 것이지만 여기서는
        # **진짜 터미널의 폭 계산**을 지난 뒤에도 남아 있는지가 요점이다.
        if read_until(primary, "zyris-code", buf, time.time() + 5, "작업 디렉터리"):
            print("  ✓ 사이드바가 작업 디렉터리를 보여준다")
            checks += 1
        else:
            ok = False

        # 6. Ctrl+C 한 번으로는 안 끝나고, 예고가 뜬다.
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

        # 7. 1.5초 안에 한 번 더 누르면 끝난다.
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

        # 8. 대체 화면을 되돌렸다. 안 되돌리면 셸이 망가진 채 남는다.
        raw = "".join(buf)
        # 나가면서 남긴 바이트도 읽어 둔다.
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
