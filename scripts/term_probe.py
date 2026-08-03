#!/usr/bin/env python3
"""터미널이 글자를 **몇 칸으로 그리는지** 직접 물어본다.

zyris-code는 `unicode-width`가 말하는 폭대로 화면을 배치한다. 터미널이 다르게 그리면
그 줄부터 오른쪽이 밀리고, ratatui는 "안 바뀐 칸"이라 여겨 다시 그리지 않으므로 깨진
자리가 그대로 남는다. **SSH로 붙었을 때만 깨진다면 이 값이 로컬과 다른지 먼저 본다.**

쓰는 법: SSH로 들어간 터미널에서 `python3 scripts/term_probe.py`.

원리: 글자를 하나 찍고 `ESC[6n`으로 커서 열을 되물어, 커서가 몇 칸 나아갔는지 센다.
"""

import os
import select
import sys
import termios
import tty

# 왼쪽이 zyris-code가 쓰는 글자, 오른쪽이 unicode-width가 말하는 폭.
GLYPHS = [
    ("●", 1, "작업 표시 점"),
    ("·", 1, "모드·에이전트 사이"),
    ("─", 1, "가름선"),
    ("│", 1, "사이드바 경계"),
    ("▌", 1, "인용 막대"),
    ("▸", 1, "도구 접힘 표시"),
    ("▾", 1, "도구 펼침 표시"),
    ("⎿", 1, "도구 결과 표시"),
    ("…", 1, "말줄임"),
    ("✓", 1, "체크"),
    ("한", 2, "한글(대조군)"),
    ("a", 1, "영문(대조군)"),
]


def ask_column(fd: int) -> int | None:
    """지금 커서가 몇 번째 열인지 터미널에 묻는다. 1부터 센다."""
    os.write(fd, b"\x1b[6n")
    buf = b""
    while b"R" not in buf:
        # 대답하지 않는 터미널이 있다. 무한정 기다리지 않는다.
        if not select.select([fd], [], [], 0.5)[0]:
            return None
        buf += os.read(fd, 32)
    try:
        return int(buf.split(b"[")[1].split(b";")[1].rstrip(b"R"))
    except (IndexError, ValueError):
        return None


def main() -> int:
    if not sys.stdin.isatty():
        print("터미널에서 직접 실행해야 합니다.")
        return 2

    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    results = []
    try:
        tty.setraw(fd)
        for glyph, expected, what in GLYPHS:
            os.write(fd, b"\r")
            before = ask_column(fd)
            os.write(fd, glyph.encode())
            after = ask_column(fd)
            os.write(fd, b"\r\x1b[K")
            if before is None or after is None:
                results.append((glyph, expected, None, what))
            else:
                results.append((glyph, expected, after - before, what))
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)

    print(f"TERM={os.environ.get('TERM', '?')}  "
          f"SSH={'예' if os.environ.get('SSH_CONNECTION') else '아니오'}")
    print(f"{'글자':<4} {'우리가 센 폭':>12} {'터미널이 그린 폭':>16}   설명")
    bad = 0
    for glyph, expected, actual, what in results:
        mark = " "
        if actual is None:
            mark = "?"
        elif actual != expected:
            mark = "← 어긋남"
            bad += 1
        print(f"{glyph:<4} {expected:>12} {str(actual):>16}   {what} {mark}")

    if bad:
        print(f"\n{bad}개가 어긋납니다. 이 글자들이 지나가는 줄부터 화면이 밀립니다.")
        print("터미널 설정에서 'ambiguous width'(모호한 문자 폭)를 1칸/narrow로 두면 맞습니다.")
    else:
        print("\n다 맞습니다. 깨짐의 원인은 글자 폭이 아닙니다.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
