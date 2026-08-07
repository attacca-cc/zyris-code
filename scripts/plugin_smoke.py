#!/usr/bin/env python3
"""`/plugin`을 진짜 pty에서 한 바퀴 돌린다. **턴은 안 돌리므로 크레딧을 안 쓴다.**

받아 올 원본으로 임시 git 리포를 만들고, 그것을 받아서 목록·갱신·삭제까지 본다.
판정은 화면과 **디스크** 양쪽이다 — 화면이 "받았습니다"라고 해도 파일이 없으면 거짓이다.

**실제 홈에 받는다.** `HOME`을 바꾸면 등록 자격이 없어 앱이 아예 안 뜬다. 그래서 끝에
반드시 치운다 — 중간에 죽어도 `finally`가 지운다.

```bash
python3 scripts/plugin_smoke.py
```
"""
import fcntl, os, pty, re, select, shutil, signal, struct, subprocess, sys, termios, time
BIN = os.path.abspath("target/debug/zyris-code")
WORK = "/tmp/zyris-code-plugin/작업"
ORIGIN = "/tmp/zyris-code-plugin/원본플러그인"
HOME = os.path.expanduser("~")
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]|\x1b\]\d+;[^\x07]*\x07")

shutil.rmtree("/tmp/zyris-code-plugin", ignore_errors=True)
os.makedirs(WORK); os.makedirs(ORIGIN)
open(f"{ORIGIN}/plugin.json","w").write(
    '{"name":"시험플러그인","description":"돌아가는지 본다",'
    '"mcpServers":{"echo":{"command":"npx","args":["-y","x"]}}}')
os.makedirs(f"{ORIGIN}/skills/리뷰")
open(f"{ORIGIN}/skills/리뷰/SKILL.md","w").write("---\nname: 리뷰\ndescription: 본다\n---\n\n1. 본다\n")
env0 = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
for a in (["init","-q","-b","main"],["add","-A"],["commit","-qm","first"]):
    subprocess.run(["git"]+a, cwd=ORIGIN, env=env0, check=True, capture_output=True)

env = dict(os.environ, ZYRIS_PROFILE="zyris-code",
           ZYRIS_CODE_LOG="/tmp/zyris-code-plugin.log")
p, r = pty.openpty()
p_fd = p
fcntl.ioctl(r, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 120, 0, 0))
proc = subprocess.Popen([BIN], stdin=r, stdout=r, stderr=r, env=env, cwd=WORK, close_fds=True)
os.close(r)
buf, ok, n = [], True, 0
def until(needle, secs, label):
    t = time.time() + secs
    while True:
        if needle in ANSI.sub("", "".join(buf)): return True
        if time.time() >= t: print(f"  ✗ {label}: '{needle}' 못 봤다"); return False
        rr,_,_ = select.select([p],[],[],0.5)
        if rr: buf.append(os.read(p,65536).decode("utf-8","replace"))
def check(c, label):
    global ok, n
    if c: print(f"  ✓ {label}"); n += 1
    else: print(f"  ✗ {label}"); ok = False
def drain(secs):
    """Sleeps while keeping the pty drained.

    **Never sleep without reading.** The buffer fills, the app blocks part-way through a draw,
    and the keys already sent sit unprocessed — which looks exactly like "the command did
    nothing". CLAUDE.md records the same trap taking out two other scripts at once.
    """
    end = time.time() + secs
    while time.time() < end:
        rr, _, _ = select.select([p_fd], [], [], 0.1)
        if rr:
            buf.append(os.read(p_fd, 262144).decode("utf-8", "replace"))

def send(t):
    os.write(p_fd, t.encode()); drain(0.6); os.write(p_fd, b"\r"); drain(1.5)

def snapshot(secs=2.0):
    """Forces a full frame and returns the whole screen.

    **ratatui only puts changed cells on the wire.** A word can therefore arrive split across
    several writes with cursor jumps between them, and a needle searched in the raw stream is
    found or missed depending on timing — `시험플러그인` really did come through as `…그인` and
    failed a check on a working install. Ctrl+L (`Action::Repaint`) redraws everything and
    changes no state, so what comes back is a screen rather than a diff.

    **Only with nothing modal open.** With a panel or the command list up, `on_key` takes the
    keystroke as input instead.
    """
    buf.clear()
    os.write(p_fd, b"\x0c")
    end = time.time() + secs
    while time.time() < end:
        rr, _, _ = select.select([p_fd], [], [], 0.2)
        if rr:
            buf.append(os.read(p_fd, 262144).decode("utf-8", "replace"))
    return ANSI.sub("", "".join(buf))


def close_panel():
    """**A listing is a popup panel now, and it is modal.**

    `/plugin` with no argument opens `panel::plugins`, which swallows every key but Esc — so the
    next command typed with one still up goes nowhere at all. This script was written before the
    panels existed and silently fed `/plugin add` to the list panel.
    """
    os.write(p, b"\x1b"); time.sleep(0.6)

try:
    if not until("일반", 30, "첫 프레임"): sys.exit(1)
    print("  ✓ 떴다"); n += 1
    time.sleep(2)
    buf.clear(); send("/plugin")
    check(until("플러그인이 없습니다", 8, "빈 목록"), "처음엔 비어 있다고 말한다")
    close_panel()
    buf.clear(); send(f"/plugin add {ORIGIN}")
    until("받았습니다", 25, "설치")
    shown = snapshot()
    check("시험플러그인" in shown, "로컬 리포에서 받는다")
    check("npx" in shown, "무슨 명령을 돌릴지 알려준다")
    check("스킬" in shown, "스킬이 딸린 것을 알려준다")
    check(os.path.exists(f"{HOME}/.config/zyris-code/plugins/원본플러그인/plugin.json"), "디스크에 실제로 놓인다")
    buf.clear(); send("/plugin")
    check(until("시험플러그인", 8, "목록"), "목록에 나온다")
    close_panel()
    buf.clear(); send("/plugin update")
    check(until("최신", 20, "갱신"), "갱신하면 이미 최신이라고 말한다")
    buf.clear(); send("/plugin remove 시험플러그인")
    check(until("지웠습니다", 10, "삭제"), "보이는 이름으로 지운다")
    check(not os.path.exists(f"{HOME}/.config/zyris-code/plugins/원본플러그인"), "디스크에서 실제로 사라진다")
    buf.clear(); send("/plugin add 없는사람/없는리포")
    check(until("git", 25, "실패 사유") or until("찾", 3, "x"), "못 받으면 사유를 말한다")
finally:
    if proc.poll() is None: proc.send_signal(signal.SIGKILL)
    os.close(p)
    # The real home was used, so it must be cleaned up.
    shutil.rmtree(f"{HOME}/.config/zyris-code/plugins/원본플러그인", ignore_errors=True)
print(f"\n{n}/11 통과")
sys.exit(0 if ok and n == 11 else 1)
