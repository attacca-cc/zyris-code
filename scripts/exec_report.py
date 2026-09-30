#!/usr/bin/env python3
"""Report what `terminal.exec` answers cost, read out of the zyris-code app log.

`exec` is the costliest tool in the token budget, and the app log is the only place its
answers are sized. `Gate::dispatch` (`crates/zyris-code/src/tools/guard.rs`) writes:

    ... INFO zyris_code::tools::guard: exec result bytes=20078 stdout=20050 \
        stderr=0 exit_code=0 command="git diff"
    ... INFO zyris_code::tools::guard: exec output cut job=x7 full=20078
    ... INFO zyris_code::tools::guard: took a tool call capability="wait" tool="logs"

`exec result` is the answer **as the agent receives it** — after `tools::clean` took the
terminal's noise off and after the 8 KB budget (`tools::budget`) shaped it. `exec output
cut` is written just before it, and only for an answer the budget cut, carrying the size of
the whole output. Pairing the two is what makes the saving visible: of a 20 KB diff only the
head, the tail and a marker ever reach the context.

Usage
-----
    python3 scripts/exec_report.py                    # $ZYRIS_CODE_LOG, else the temp dir
    python3 scripts/exec_report.py --pid 4242         # one window; default: the latest one
    python3 scripts/exec_report.py --all              # every window in the log together
    python3 scripts/exec_report.py one.log two.log    # per log, then a combined total
    python3 scripts/exec_report.py --out report.md    # write it somewhere another tool can read
    python3 scripts/exec_report.py --json data.json

The saving reported is the **budget's** alone: what `tools::clean` removed (ANSI escapes,
overwritten progress bars) is already gone by the time either line is measured.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import sys
import tempfile

MARK_CUT = "exec output cut"
MARK_RESULT = "exec result"
MARK_CALL = "took a tool call"

_INT = re.compile(r"\b(\w+)=(-?\d+)(?=\s|$)")
_STR = re.compile(r'\b(\w+)="((?:[^"\\]|\\.)*)"')
_WORD = re.compile(r'\b(capability|tool|job)=([^\s"]+)')
_CMD = re.compile(r"\bcommand=(.*)$")
_TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\S+)")
# Every line the app writes starts with the process that wrote it, since windows share one log.
_PID = re.compile(r"^\[(\d+)\] ")


def this_run(lines: list[str], pid: int | None, everything: bool) -> tuple[list[str], int | None]:
    """The lines of one window, prefix taken off, and which window that was.

    **Several windows append to one log**, so reading it whole would mix their sessions. The
    default is the window that wrote last — the one just closed. Lines from a build that wrote
    no prefix belong to no window and are kept only with `--all`.
    """
    if everything:
        return [_PID.sub("", line) for line in lines], None
    if pid is None:
        for line in reversed(lines):
            m = _PID.match(line)
            if m:
                pid = int(m.group(1))
                break
        if pid is None:
            return lines, None  # a log from before the prefix: one run, as it used to be
    prefix = f"[{pid}] "
    return [line[len(prefix):] for line in lines if line.startswith(prefix)], pid

# The app writes the cut line immediately before the result it belongs to, so a handful
# of lines is the whole window it can be paired across.
PAIR_WINDOW = 6


def default_log() -> str:
    return os.environ.get("ZYRIS_CODE_LOG") or os.path.join(
        tempfile.gettempdir(), "zyris-code.log"
    )


def fields(line: str) -> dict:
    """Every `key=value` on the line, in whatever order the layer printed them."""
    out: dict = {}
    for key, value in _INT.findall(line):
        out[key] = int(value)
    for key, value in _STR.findall(line):
        try:
            out[key] = json.loads(f'"{value}"')
        except ValueError:
            out[key] = value
    for key, value in _WORD.findall(line):
        out.setdefault(key, value)
    return out


def parse_result(line: str) -> dict:
    # Numbers are read off the part before `command=`: a command can contain anything,
    # including something that looks like a field (`grep bytes= x`).
    m = _CMD.search(line)
    head = line[: m.start()] if m else line
    f = fields(head)
    command = ""
    if m:
        command = m.group(1).strip()
        # A message may be sitting after the command on the line, quoted or not. Take it off
        # first, so the closing quote of the command itself is not eaten with it.
        command = re.sub(r'\s*(?:exec result|exec output cut)\s*$', "", command).strip()
        if len(command) > 1 and command[0] == '"' and command[-1] == '"':
            try:
                command = json.loads(command)
            except ValueError:
                command = command[1:-1]
    return {
        "bytes": f.get("bytes", 0),
        "stdout": f.get("stdout", 0),
        "stderr": f.get("stderr", 0),
        "exit_code": f.get("exit_code", -1),
        "command": command,
        "full": None,
        "job": None,
    }


def read_log(path: str, pid: int | None = None, everything: bool = False) -> dict:
    """Everything one app log says about exec, for one window unless `everything`."""
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        lines, pid = this_run(fh.read().splitlines(), pid, everything)

    results: list[dict] = []
    calls: dict = {}
    first_ts = last_ts = None
    pending = None  # (job, full, line number)

    for number, line in enumerate(lines):
        stripped = line.strip()
        m = _TS.match(stripped)
        if m:
            if first_ts is None:
                first_ts = m.group(1)
            last_ts = m.group(1)
        # Only the part before `command=` is looked at for a marker: a command's own text is
        # arbitrary, and `cat > f <<'EOF'` with a sample log inside it is a line that contains
        # the very words this looks for.
        head = line.split("command=", 1)[0]
        if MARK_CUT in head and MARK_RESULT not in head:
            f = fields(head)
            pending = (f.get("job"), f.get("full"), number)
        elif MARK_CALL in head:
            f = fields(head)
            key = f"{f.get('capability', '?')}.{f.get('tool', '?')}"
            calls[key] = calls.get(key, 0) + 1
        elif MARK_RESULT in head:
            result = parse_result(line)
            if pending is not None and number - pending[2] <= PAIR_WINDOW:
                result["job"], result["full"] = pending[0], pending[1]
                pending = None
            results.append(result)

    cut = [r for r in results if r.get("full")]
    # Content as the agent receives it, and content of the same session with no budget: for
    # a cut call that is the whole output the cut line recorded, otherwise what it delivered.
    delivered = sum(r["stdout"] + r["stderr"] for r in results)
    unbudgeted = sum(
        (r["full"] if r.get("full") else r["stdout"] + r["stderr"]) for r in results
    )
    return {
        "path": path if pid is None else f"{path} [{pid}]",
        "lines": len(lines),
        "first": first_ts,
        "last": last_ts,
        "calls": calls,
        "results": results,
        "cut": cut,
        "delivered": delivered,
        "unbudgeted": unbudgeted,
        "wire": sum(r["bytes"] for r in results),
        "median_wire": int(statistics.median([r["bytes"] for r in results]))
        if results
        else 0,
        "median_content": int(
            statistics.median([r["stdout"] + r["stderr"] for r in results])
        )
        if results
        else 0,
    }


def num(value) -> str:
    return f"{int(value):,}"


def tokens(value: int) -> str:
    """Bytes to tokens, at the rough 4 bytes/token the log's own numbers suggest."""
    return f"~{int(value) // 4:,}"


def pct(part: int, whole: int) -> str:
    return "n/a" if whole <= 0 else f"{part * 100 / whole:.1f}%"


def clip(text: str, width: int = 60) -> str:
    text = text.replace("|", "\\|")
    return text if len(text) <= width else text[: width - 1] + "…"


def render(log: dict, top: int) -> list[str]:
    out: list[str] = [f"### `{log['path']}`", ""]
    if log["first"]:
        out += [f"{log['first']} → {log['last']} · {num(log['lines'])} lines", ""]

    results, cut = log["results"], log["cut"]
    if not results:
        out += [
            "No `exec result` line in this log. The app writes one per `terminal.exec` call,"
            " so either no exec ran in this session or the build predates PR #39.",
            "",
        ]
        return out

    saved = max(log["unbudgeted"] - log["delivered"], 0)
    out += [
        "| | value |",
        "| --- | ---: |",
        f"| `terminal.exec` answers logged | {num(len(results))} |",
        f"| answers the budget cut | {num(len(cut))} |",
        f"| answer content delivered | {num(log['delivered'])} B ({tokens(log['delivered'])} tok) |",
        f"| the same content with no budget | {num(log['unbudgeted'])} B ({tokens(log['unbudgeted'])} tok) |",
        f"| **saved by the budget** | **{num(saved)} B ({tokens(saved)} tok), {pct(saved, log['unbudgeted'])}** |",
        f"| median answer (content / on the wire) | {num(log['median_content'])} B / {num(log['median_wire'])} B |",
        f"| largest answer on the wire | {num(max(r['bytes'] for r in results))} B |",
        "",
    ]
    if cut:
        out += [
            "**The calls the budget cut** — what each one's whole output was worth:",
            "",
            "| command | whole output | delivered | saved | exit | job |",
            "| --- | ---: | ---: | ---: | ---: | --- |",
        ]
        for r in sorted(cut, key=lambda r: -(r["full"] or 0)):
            got = r["stdout"] + r["stderr"]
            out.append(
                f"| `{clip(r['command'])}` | {num(r['full'])} B | {num(got)} B | "
                f"{num(max((r['full'] or 0) - got, 0))} B | {r['exit_code']} | `{r['job']}` |"
            )
        out.append("")

    out += [
        f"**The {min(top, len(results))} most expensive answers** (on the wire):",
        "",
        "| command | bytes | tokens | exit |",
        "| --- | ---: | ---: | ---: |",
    ]
    for r in sorted(results, key=lambda r: -r["bytes"])[:top]:
        out.append(
            f"| `{clip(r['command'])}` | {num(r['bytes'])} | {tokens(r['bytes'])} | {r['exit_code']} |"
        )
    out.append("")

    if log["calls"]:
        total = sum(log["calls"].values())
        exec_calls = log["calls"].get("terminal.exec", 0)
        follow = log["calls"].get("wait.logs", 0)
        out += [
            "**Every tool call in the log** — and whether the agent pages back what was cut:",
            "",
            "| tool | calls | share |",
            "| --- | ---: | ---: |",
        ]
        for key, count in sorted(log["calls"].items(), key=lambda kv: -kv[1])[:12]:
            out.append(f"| `{key}` | {num(count)} | {pct(count, total)} |")
        out += [f"| **all** | **{num(total)}** | 100.0% |", ""]
        out += [
            "The call counts come from `took a tool call` lines, the exec totals above from"
            " `exec result` lines. **The two can differ**: a call that never produced a plain"
            " answer — refused by the gate, or still running when the log was read — has the"
            " first line and not the second.",
            "",
        ]
        if exec_calls:
            out += [
                f"`terminal.exec` is {pct(exec_calls, total)} of the calls. `wait.logs` was"
                f" called {num(follow)} times against {num(len(cut))} cut answers — if the agent"
                " pages the whole output back every time, the budget saves nothing and the"
                " steering has to carry the weight.",
                "",
            ]
    return out


def render_total(logs: list[dict]) -> list[str]:
    out = [
        "### All logs together",
        "",
        "| log | exec calls | cut | content, no budget | delivered | saved | saved % | saved tokens |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for log in logs:
        saved = max(log["unbudgeted"] - log["delivered"], 0)
        out.append(
            f"| `{os.path.basename(log['path'])}` | {num(len(log['results']))} |"
            f" {num(len(log['cut']))} | {num(log['unbudgeted'])} | {num(log['delivered'])} |"
            f" {num(saved)} | {pct(saved, log['unbudgeted'])} | {tokens(saved)} |"
        )
    unbudgeted = sum(log["unbudgeted"] for log in logs)
    delivered = sum(log["delivered"] for log in logs)
    saved = max(unbudgeted - delivered, 0)
    out += [
        f"| **total** | **{num(sum(len(l['results']) for l in logs))}** |"
        f" **{num(sum(len(l['cut']) for l in logs))}** | **{num(unbudgeted)}** |"
        f" **{num(delivered)}** | **{num(saved)}** | **{pct(saved, unbudgeted)}** |"
        f" **{tokens(saved)}** |",
        "",
    ]
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("logs", nargs="*", help="app log(s); default $ZYRIS_CODE_LOG")
    ap.add_argument("--out", help="write the report here as well")
    ap.add_argument("--json", help="write the parsed numbers here")
    ap.add_argument("--top", type=int, default=10, help="how many of the largest answers")
    ap.add_argument("--pid", type=int, help="the window to report on; default the latest")
    ap.add_argument("--all", action="store_true", help="every window in the log together")
    args = ap.parse_args(argv)

    paths = args.logs or [default_log()]
    logs, missing = [], []
    for path in paths:
        if not os.path.exists(path):
            missing.append(path)
            continue
        logs.append(read_log(path, args.pid, args.all))
    for path in missing:
        print(f"no such log: {path}", file=sys.stderr)
    if not logs:
        print(
            "nothing to read — name the log, or set ZYRIS_CODE_LOG to where the app writes it",
            file=sys.stderr,
        )
        return 1

    out = ["# What `terminal.exec` costs", ""]
    for log in logs:
        out += render(log, args.top)
    if len(logs) > 1:
        out += render_total(logs)
    out.append(
        "Tokens are the rough `bytes / 4`: the log records bytes, not tokens."
    )
    text = "\n".join(out).rstrip() + "\n"
    print(text)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(text)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(logs, fh, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
