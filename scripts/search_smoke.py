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
# **Say the tool name in its real form.** The wire name is
# `zyris__{node}__{capability}__{tool}`, so calling it `search.grep` makes the model fail to find it.
ASK = (
    "두 가지를 순서대로 하라. "
    "(1) 이름이 '__search__grep'으로 끝나는 도구를 pattern='자물쇠를여는열쇠'로 호출한다. "
    "(2) 이름이 '__code_edit__write'로 끝나는 도구로 path='결과.txt'에 "
    "(1)에서 찾은 파일 경로를 그대로 써 넣는다. 설명은 하지 마라."
)
# **The verdict is made here.** Not what the agent said, but what remains on disk.
OUT = "결과.txt"
# A token that is nowhere else. If it happens to match elsewhere, the check becomes a false positive.
NEEDLE = "자물쇠를여는열쇠"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]")


def read_until(fd, needle, buf, deadline, label):
    while True:
        # `needle` may be a tuple; then **any one** of them passes. Used for what the person's
        # settings can change, such as the mode label in the bottom bar.
        wanted = (needle,) if isinstance(needle, str) else tuple(needle)
        if any(w in ANSI.sub("", "".join(buf)) for w in wanted):
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
    # **Also checks that `.gitignore` actually applies.** Its contents must not appear in the result.
    with open(os.path.join(SCRATCH, ".gitignore"), "w") as f:
        f.write("target/\n")
    os.makedirs(os.path.join(SCRATCH, "target"))
    with open(os.path.join(SCRATCH, "target", "숨을것.rs"), "w") as f:
        f.write(f"let {NEEDLE} = 2;\n")
    print(f"작업 디렉터리: {SCRATCH}\n")

    env = dict(
        os.environ,
        ZYRIS_CODE_LANG="ko",
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
        if not read_until(primary, ("일반", "계획", "작업", "일"), buf, time.time() + 30, "첫 프레임"):
            return finish(proc, primary, checks, total, False)
        print("  ✓ 떴다")
        checks += 1

        # Sending immediately after connect could catch an empty tool list — the server
        # snapshots capabilities within 500ms after the handshake.
        time.sleep(3)
        os.write(primary, ASK.encode())
        time.sleep(1.0)
        os.write(primary, b"\r")

        # **This is the only verdict.** Do not read the moment it appears — the agent has
        # actually written a placeholder first and then fixed it (we caught `PLACEHOLDER`).
        out = os.path.join(SCRATCH, OUT)
        found = ""
        deadline = time.time() + 240
        while time.time() < deadline:
            time.sleep(0.5)
            # Keep draining the screen too — if it is not read, the pty buffer fills and the app freezes.
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

        # Nothing caught by `.gitignore` may appear in the result.
        if "숨을것" in found:
            print("  ✗ gitignore에 걸린 파일이 결과에 나왔다")
            ok = False
        else:
            print("  ✓ gitignore에 걸린 파일은 안 나온다")
            checks += 1

        # **Nothing is asked.** Approval was removed, so any window appearing is a regression.
        if "승인이 필요합니다" not in ANSI.sub("", "".join(buf)):
            print("  ✓ 아무것도 묻지 않는다")
            checks += 1
        else:
            print("  ✗ 없앤 승인 창이 떴다")
            ok = False

        # **`/undo` is also judged by its side effect.** The edit created a file that did not exist, so undoing deletes it.
        if not os.path.exists(out):
            print("  ✗ 되돌릴 파일이 없어 /undo를 못 봤다")
            ok = False
        else:
            # **May need several presses.** If the agent wrote the same file twice, one undo
            # only goes back one version — walk back until it is gone.
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
