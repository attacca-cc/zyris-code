#!/usr/bin/env python3
"""창 둘을 진짜로 띄워 서로 다른 노드로 붙는지 본다.

**판정은 부작용과 로그로만 한다.** 화면에 뭐라고 쓰여 있느냐가 아니라, 서버가 답한
`node_id`가 창마다 다른가 하나다 — 같으면 레지스트리가 덮이고 앞 창은 도구 호출을
하나도 못 받는다(CLAUDE.md `### 창 여럿`).

돌리는 것: 창 1을 띄워 붙기를 기다리고, 창 2를 띄운다. 창 2는 자기 몫의 노드가 없으므로
요청 파일을 남기고, 창 1이 그것을 보고 `register_node`로 만들어 준다. 그 뒤 창 2가
받은 토큰으로 붙는다.

**턴을 안 돌리므로 크레딧을 안 쓴다.** 다만 계정에 노드가 하나 늘어난다(슬롯 2) —
그 노드는 앞으로 두 번째 창이 계속 재사용한다.
"""

import json
import os
import pty
import re
import select
import subprocess
import sys
import time
from pathlib import Path

BIN = "target/debug/zyris-code"
CONFIG = Path.home() / ".config" / "zyris-code"
CONNECTED = re.compile(r"connected node_id=([0-9a-f-]+)")
NAMED = re.compile(r"starting zyris node node=(.+?) url=")


def spawn(log_path):
    """앱을 pty에 띄운다. **크기를 반드시 정한다** — 0×0이면 ratatui가 아무것도 안 그린다."""
    import fcntl
    import struct
    import termios

    parent, child = pty.openpty()
    fcntl.ioctl(parent, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    env = dict(os.environ, ZYRIS_CODE_LOG=str(log_path), ZYRIS_CODE_LANG="ko")
    proc = subprocess.Popen(
        [BIN], stdin=child, stdout=child, stderr=child, env=env, close_fds=True
    )
    os.close(child)
    return proc, parent


def drain(fd, seconds):
    """**pty를 계속 빨아들인다.** 안 읽으면 버퍼가 차서 앱이 그리다 멈춘다."""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                os.read(fd, 65536)
            except OSError:
                return


def wait_for_node(log_path, fd, timeout):
    """로그가 `connected node_id=…`를 적을 때까지, pty를 빨면서 기다린다."""
    end = time.time() + timeout
    while time.time() < end:
        drain(fd, 0.5)
        if log_path.exists():
            found = CONNECTED.findall(log_path.read_text(errors="replace"))
            if found:
                return found[-1]
    return None


def name_in(log_path):
    text = log_path.read_text(errors="replace") if log_path.exists() else ""
    found = NAMED.findall(text)
    return found[-1] if found else "?"


def main():
    logs = [Path("/tmp/zyris-window-1.log"), Path("/tmp/zyris-window-2.log")]
    for log in logs:
        log.unlink(missing_ok=True)

    procs = []
    ok = True
    try:
        print("창 1 …")
        p1, fd1 = spawn(logs[0])
        procs.append((p1, fd1))
        node1 = wait_for_node(logs[0], fd1, 60)
        print(f"  node_id={node1}  name={name_in(logs[0])}")
        if not node1:
            print("  창 1이 붙지 못했다. 여기서 멈춘다.")
            return 1

        print("창 2 …")
        p2, fd2 = spawn(logs[1])
        procs.append((p2, fd2))
        # 창 1이 요청을 보려면 한 바퀴(2초)가 필요하고, 등록에 왕복이 하나 더 든다.
        node2 = wait_for_node(logs[1], fd2, 60)
        print(f"  node_id={node2}  name={name_in(logs[1])}")

        child = CONFIG / "node-2.json"
        if child.exists():
            saved = json.loads(child.read_text())
            print(f"  캐시: {child.name}  node_id={saved['node_id']}  name={saved['name']}")
            print(f"  토큰: {'있음' if saved.get('token') else '없음'}")
        else:
            print(f"  캐시 없음: {child}")
            ok = False

        if not node2:
            print("  ✗ 창 2가 붙지 못했다")
            ok = False
        elif node1 == node2:
            print("  ✗ 두 창이 같은 노드다 — 레지스트리가 덮인다")
            ok = False
        else:
            print("  ✓ 두 창이 서로 다른 노드로 붙었다")

        # 창 1이 아직 살아 있는가. 밀려났으면 여기서 죽어 있거나 재연결 로그가 있다.
        print(f"  창 1 살아 있음: {p1.poll() is None}")
    finally:
        for proc, fd in procs:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            os.close(fd)

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
