#!/usr/bin/env python3
"""실제 pty에서 **에이전트가 `search`를 부르는지** 본다. 크레딧을 쓴다.

**판정은 디스크로만 한다.** 에이전트가 뭐라고 말했는지도, 화면에 무엇이 떴는지도 보지
않는다. 화면을 보려다 한 번 걸렸다 — `"grep"`을 찾았는데 그 글자가 **내가 보낸 프롬프트
안에** 있어서, 도구가 돌지도 않았는데 통과했다. 프롬프트가 도구 이름을 말해 줘야 하는
이상 화면 검사는 늘 그 함정을 안고 있다.

그래서 판정은 하나다: 에이전트가 찾은 경로를 파일에 쓰게 하고, **그 파일 내용을 본다.**
찾지 못했으면 쓸 수 없는 값이다.

**승인은 없앴다.** 승인 창이 뜨면 그것 자체가 회귀다 — 그것도 검사한다.

```bash
python3 scripts/search_smoke.py
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
SCRATCH = "/tmp/zyris-code-search"
# **도구 이름을 실제 모양으로 말해 준다.** 와이어 이름은
# `zyris__{노드}__{캐퍼빌리티}__{도구}`라 `search.grep`이라고 부르면 모델이 못 찾는다.
ASK = (
    "두 가지를 순서대로 하라. "
    "(1) 이름이 '__search__grep'으로 끝나는 도구를 pattern='자물쇠를여는열쇠'로 호출한다. "
    "(2) 이름이 '__code_edit__write'로 끝나는 도구로 path='결과.txt'에 "
    "(1)에서 찾은 파일 경로를 그대로 써 넣는다. 설명은 하지 마라."
)
# **판정은 여기서 한다.** 에이전트가 무슨 말을 했는지가 아니라 디스크에 무엇이 남았는지다.
OUT = "결과.txt"
# 어디에도 없을 낱말. 다른 데서 우연히 걸리면 검사가 거짓 양성이 된다.
NEEDLE = "자물쇠를여는열쇠"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")


def read_until(fd, needle, buf, deadline, label):
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
    os.makedirs(os.path.join(SCRATCH, "src"))
    with open(os.path.join(SCRATCH, "src", "찾을것.rs"), "w") as f:
        f.write(f"fn a() {{}}\nlet {NEEDLE} = 1;\n")
    # **`.gitignore`가 실제로 걸리는지도 본다.** 여기 것이 결과에 나오면 안 된다.
    with open(os.path.join(SCRATCH, ".gitignore"), "w") as f:
        f.write("target/\n")
    os.makedirs(os.path.join(SCRATCH, "target"))
    with open(os.path.join(SCRATCH, "target", "숨을것.rs"), "w") as f:
        f.write(f"let {NEEDLE} = 2;\n")
    print(f"작업 디렉터리: {SCRATCH}\n")

    env = dict(
        os.environ,
        ZYRIS_PROFILE="zyris-code",
        ZYRIS_CODE_LOG="/tmp/zyris-code-search.log",
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
    total = 5

    try:
        if not read_until(primary, "기본", buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        # 붙자마자 보내면 도구 목록이 비어 있을 수 있다 — 서버는 handshake 뒤 500ms 안에
        # capability를 스냅숏한다.
        time.sleep(3)
        os.write(primary, ASK.encode())
        time.sleep(1.0)
        os.write(primary, b"\r")

        # **여기가 유일한 판정이다.** 비지 않자마자 읽으면 안 된다 — 에이전트가
        # 자리표시자를 먼저 쓰고 고치는 일이 실제로 있었다(`PLACEHOLDER`를 잡았다).
        out = os.path.join(SCRATCH, OUT)
        found = ""
        deadline = time.time() + 240
        while time.time() < deadline:
            time.sleep(0.5)
            # 화면도 계속 빨아들인다 — 안 읽으면 pty 버퍼가 차서 앱이 멈춘다.
            r, _, _ = select.select([primary], [], [], 0)
            if r:
                buf.append(os.read(primary, 65536).decode("utf-8", "replace"))
            if os.path.exists(out):
                found = open(out).read()
                if "찾을것" in found:
                    break

        if "찾을것" in found:
            print("  ✓ 심어 둔 파일을 찾아냈다")
            checks += 1
        else:
            print(f"  ✗ 찾은 것이 우리가 심은 파일이 아니다: {found!r}")
            ok = False

        # `.gitignore`에 걸린 것이 결과에 있으면 안 된다.
        if "숨을것" in found:
            print("  ✗ gitignore에 걸린 파일이 결과에 나왔다")
            ok = False
        else:
            print("  ✓ gitignore에 걸린 파일은 안 나온다")
            checks += 1

        # **아무것도 묻지 않는다.** 승인은 없앴으므로 창이 떴다면 회귀다.
        if "승인이 필요합니다" not in ANSI.sub("", "".join(buf)):
            print("  ✓ 아무것도 묻지 않는다")
            checks += 1
        else:
            print("  ✗ 없앤 승인 창이 떴다")
            ok = False

        # **`/undo`도 부작용으로 판정한다.** 없던 파일을 만든 편집이므로 되돌리면 지워진다.
        if not os.path.exists(out):
            print("  ✗ 되돌릴 파일이 없어 /undo를 못 봤다")
            ok = False
        else:
            # **여러 번 눌러야 할 수 있다.** 에이전트가 같은 파일을 두 번 썼으면 한 번
            # 되돌리기는 앞 판으로 돌아갈 뿐이다 — 없어질 때까지 거슬러 올라간다.
            time.sleep(1.0)
            gone = False
            for _ in range(4):
                os.write(primary, b"/undo")
                time.sleep(0.5)
                os.write(primary, b"\r")
                for _ in range(20):
                    time.sleep(0.25)
                    if not os.path.exists(out):
                        gone = True
                        break
                if gone:
                    break
            if gone:
                print("  ✓ `/undo`가 만든 파일을 지운다")
                checks += 1
            else:
                print("  ✗ 되돌렸는데 파일이 남아 있다")
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
