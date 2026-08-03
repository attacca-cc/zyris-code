# zyris-code

A terminal coding client for [Attacca](https://attacca.cc) agents, built with
[ratatui](https://ratatui.rs) and connected over the
[Zyris](https://github.com/attacca-cc/zyris) protocol.

It is not just a chat window. **It hands your machine to the agent**: reading and
editing files, searching the tree, running shell commands, and exposing the tools
of any local MCP server you configure. The agent runs on Attacca; the tools run
here.

> The interface itself is currently Korean-only. Mode names and prompts are given
> in both languages below where it matters.

```
▌ Where does the scroll window get computed?

▾ Looking for the scroll math  ·  3 steps  +12 −3
┊ Start with the layout, then follow the viewport
● grep  scroll.window
● edit  src/rows.rs           +12 −3  ▸

◆ `rows.rs` owns it. `window(start, end)` builds only the visible
  lines, so the count and the draw can never disagree.
```

## Running

```bash
cargo run -p zyris-code
```

On first launch an 8-digit enrollment code is shown in a panel with the URL to
open. Enter it on any device with a browser to pair this node with your Attacca
account; it continues on its own once you approve. Credentials are stored in
`~/.config/zyris-code/wss-<server>-<profile>.json` — this client's own
directory, not one shared with every other zyris program on the machine. A
credential left behind by an older build is moved across on first run.

Approve **every** scope on that screen. Scopes are fixed at approval time and
refreshing never widens them, so a grant missing `agents:read` or
`projects:read` shows an empty agent or project list rather than an error.
zyris-code names what is missing and asks once for a fresh code.

The first enrolment registers as `<hostname> zyris-code` rather than the bare
hostname, so it stays distinct from any other zyris node on the same machine.
Override with `ZYRIS_NODE_NAME`; `/cwd` shows what this window registered as.

**Open as many windows as you like.** Nothing stops a second one, in the same
directory or another. What you should know is that this deployment gives one
credential exactly one node: connecting twice returns the same `node_id`, and
the server routes tool calls to whichever connection arrived last. The earlier
window keeps drawing, keeps its history, and receives no tool calls.

In the same directory that changes nothing worth guarding against — whichever
window the agent reaches, the files it edits are the same. Across different
directories it matters, and so does this: **the approval prompt and the plan/edit
mode that judge a call belong to the window that received it.**

An earlier design had each window register a sibling node of its own
(`register_node`), which would have split the routing per window. The server does
not implement it — `attacca_api.register_node` answers `MethodNotFound`, and the
`nodes:write` scope it needs is rejected at enrolment — so that code is gone.
`cargo run -p zyris-code --example nodes_probe` re-measures both in a few seconds
if you want to know whether that has changed.

Machines that share a hostname (`arch`, `nixos`) each enrol separately, so they
are distinct nodes on the server and the slug is disambiguated automatically.
Only the display name collides — name them with `ZYRIS_NODE_NAME`.

If the credential is ever revoked mid-session, the enrollment code is drawn in
the UI itself — a panel over the conversation with the URL, the code, and a
countdown — and a line stays in the chat so it is still there after the panel
closes.

Run it **from the directory you want to work in** — that directory defines the
fence described below.

## The working directory is a fence

Everything inside the directory you launched from runs without interruption.
Anything that reaches outside it — reads included — stops and asks you:

```
Outside the working directory. Approval required
  /home/you/other-project/Cargo.toml
  file_io.read · running in /home/you/my-app
  y allow / n deny / a allow this directory for the session
```

- **Reads count too.** The point is to keep the agent from wandering across your
  whole disk.
- **`a` opens the whole directory**, not just that one file. Approving a
  neighbouring repo and then being asked about each file in it would be useless.
  Grants are never written to disk; quitting forgets them. `/grants` lists what
  is open and `/grants close` shuts it again — an approval that lasts all
  session and cannot be seen is worth little.
- **A shell command cannot be fenced completely.** `terminal.exec` runs an
  arbitrary program, and no amount of reading the command text tells you where it
  will go — `sh -c` alone can do anything. Absolute paths and `../` in the command
  are caught, which stops accidental escapes, but treat it as a net rather than a
  wall. Symlinks pointing outward are not followed either.

## Modes

`Shift+Tab` cycles through three modes, shown in the bottom bar. A mode decides two
things: whether tools may run, and where your next message goes.

| Mode | Tools | Your next message |
|---|---|---|
| **job** (default) | Run. The fence above still applies. | Opens an Attacca **job** — hand it over and it runs to the end. If it asks something back, answer right here. |
| **plan** (계획) | Nothing is changed and no command runs; the agent has to describe its plan first. Reading still works — you cannot plan what you cannot see. | Stays in the conversation you are already having. |
| **work** | Run, same as job. | Opens an Attacca **work**: it is planned into a task graph, each task running in its own git worktree. Two gates need a person to open them. |

`job` is the resting state. There is no separate "plain session" mode — a job *is* a
session, and it also shows up in Attacca with a state and a place in your lists.

`plan` is the only mode that stays in the current conversation, so you can switch it on
mid-thread without losing your place. `job` and `work` open something new with your
**next message only**; after that you are talking to it.

`/mode job|plan|work` does the same thing without the keyboard. `/mode normal` and
`/mode 기본` still work — they mean `job` now.

Whatever you open lands in **the project you last picked** (`←`), not the default one.

`work` and `job` are deliberately untranslated — they are what Attacca calls them, so
what you open here is what you look up there.

## What the agent gets

| Capability | Tools |
|---|---|
| `search` | `glob`, `grep` — respects `.gitignore`, skips binaries |
| `file_io` | `stat`, `list`, `read`, `read_stream` — **read only** |
| `code_edit` | `edit`, `multi_edit`, `write` |
| `terminal` | `exec`, plus a full PTY: `open`, `read`, `write`, `screen`, `resize`, `close` |
| `skill` | `list`, `load` |
| `work` | `start`, `status`, `list`, `say`, `stop`, `resume` — hands a goal to attacca |
| `mcp_*` | whatever your MCP servers expose |

There is deliberately no tool that deletes a file, and writes go through
`code_edit` only — a single path for every change means a single diff and a single
undo record.

`work` is the odd one out: it touches nothing on this machine. It hands a goal to
attacca, which plans it into a task graph and runs each task in its own git
worktree with a subagent — for work too large for one thread. Creating a work does
not start it: it stops at two gates, the goal and the plan, for a person to
approve in attacca. **Those two approvals are deliberately not exposed as tools**;
an agent that approves its own plan makes the gates pointless. `status` reports
which gate a work is waiting at so the agent can say so.

Starting, messaging, stopping and resuming a work are writes, so plan mode refuses
them; `status` and `list` are reads and go through.

## Slash commands

Type `/` and the command list opens; it narrows as you type. Commands are handled
locally and never reach the server.

| Command | Does |
|---|---|
| `/help` | List these |
| `/mode [normal\|plan]` | Show or change the mode |
| `/agent [name]` | Pick an agent (see below) |
| `/mcp` | Connected MCP servers, and why any failed |
| `/skills` | Available skills |
| `/plugin [add\|remove\|update]` | Install and manage plugins |
| `/rules` | Which `CLAUDE.md` / `AGENTS.md` files this session loaded |
| `/cwd` | Where tools resolve relative paths |
| `/grants` | Directories opened outside the fence; `/grants close` shuts them all |
| `/changes` | Files changed in this directory, with `+N −N` |
| `/undo` | Revert the last edit |
| `/clear` | Clear the screen — the session history on the server is untouched |
| `/quit` | Leave. A running turn is stopped on the server first |

`/help` also prints the key bindings — the table below is the same list, but a
README is not open at the moment you need it.

### `/agent` opens a new thread

An agent is fixed when a thread is created and there is no API to change it, so
picking a different agent **starts a new thread with your next message**. Nothing
is created on the server until then, and the previous thread stays in the picker
(`←`).

The starting agent is `Main Agent`, overridable with `ZYRIS_CODE_AGENT`. If the
name is not on your account, zyris-code says so instead of silently falling back
to a different agent.

## Undo

Every edit stores the previous contents before writing, so `/undo` walks back one
edit at a time. Backups live in `~/.cache/zyris-code/undo/<working-directory>/`,
**never inside your repository** — a directory appearing in your project would end
up in a commit sooner or later.

If a backup cannot be written the edit still goes ahead. A missing safety net is
not a reason to block work.

`/changes` reads the same record from the other end: one row per file, `+N −N`
measured from the oldest backup to what is on disk now, newest first. What it
lists is exactly what `/undo` can walk back — including edits from earlier runs,
since the record outlives the process.

## Project instructions

`CLAUDE.md` and `AGENTS.md` are loaded into the session, walking **up** from the
working directory so both a repository's own conventions and any broader rules
above it apply. Files closer to the working directory come last and win. If both
names exist in one directory, `CLAUDE.md` is used.

These are read when the session is created and cannot change afterwards, so edit
them and then start a new session. `/rules` shows what the current session got.

## Skills

A skill is a directory with a `SKILL.md` inside:

```
~/.config/zyris-code/skills/review/SKILL.md
<project>/.zyris-code/skills/deploy/SKILL.md
```

```markdown
---
name: review
description: How we review changes in this repository
---

1. …
```

Only names and descriptions go into the session; the body is fetched by
`skill.load` when the agent decides a skill applies. Loading everything up front
would spend context on procedures that never get used.

## MCP servers

Configure them in `~/.config/zyris-code/mcp.json` or `<project>/.mcp.json`. The
project file wins when both name the same server.

```json
{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_TOKEN": "…" }
    }
  }
}
```

Each server becomes a capability named `mcp_<key>` and its tools appear to the
agent alongside the built-in ones. Servers start in the background so a slow
`npx` never delays the UI, and a server that fails to start is reported rather
than silently dropped — `/mcp` shows the reason.

## Plugins

A plugin bundles MCP servers and skills together:

```
~/.config/zyris-code/plugins/my-plugin/
  plugin.json     { "name": "my-plugin", "mcpServers": { … } }
  skills/
    review/SKILL.md
```

Plugins are also read from `<project>/.zyris-code/plugins/`. A broken plugin is
logged and skipped; the rest still load.

### Installing

```
/plugin add owner/repo                       # GitHub shorthand
/plugin add https://github.com/owner/repo    # or the full URL
/plugin add ~/my-plugin                      # or a local repo, while writing one
/plugin                                      # list what is installed
/plugin update [name]                        # git pull
/plugin remove <name>
```

Installation is a `git clone` into `~/.config/zyris-code/plugins/`, never into your
project. A repository without a `plugin.json` at its root is rejected and the clone
is deleted, so a plugin that silently does nothing cannot happen.

**A plugin is somebody else's code on your machine.** Installing runs nothing, but
the next start does — the MCP commands in its manifest are launched. zyris-code
prints what a plugin will run as soon as it is installed, so read that before
restarting.

Plugins are loaded at startup, so restart to pick up a newly installed one.

## Keys

| Key | Does |
|---|---|
| `Enter` | Send |
| `Shift+Tab` | Cycle mode: job → plan → work |
| `Ctrl+O` | Fold / unfold the work card |
| `Ctrl+B` | Toggle the sidebar |
| `←` | Project and session picker |
| `↑` / `↓` | Recall previous messages |
| `Esc` | Cancel the running turn |
| `Ctrl+C` | Cancel the running turn; press again to arm quitting, once more to quit |
| `y` / `n` / `a` | Answer an approval prompt |
| Wheel · drag | Scroll · select and copy |

The second `Ctrl+C` arms quitting even while a turn is still running, so a server
that has stopped answering cannot trap you in the window.

**Closing the window stops the turn on the server.** The turn runs there, not
here — left alone it keeps thinking, fails every tool call looking for a node
that is gone, and spends credit doing it. `SIGTERM` and `SIGHUP` (closing the
terminal) take the same path. If the server does not answer within three
seconds the window closes anyway.

Work cards are folded and unfolded by hand (`Ctrl+O`) and never move on their
own — a screen that folds itself while you are reading it is worse than one card
too many.

Messages typed while a turn is running are queued and sent in order when it ends.

## Environment

| Variable | Default | Does |
|---|---|---|
| `ZYRIS_CODE_AGENT` | `Main Agent` | Agent to connect to at startup |
| `ZYRIS_NODE_NAME` | `<hostname> zyris-code` | The name this node registers under |
| `ZYRIS_PROFILE` | `zyris-code` | Credential file within that directory, so one machine can hold several identities |
| `ZYRIS_CONFIG_DIR` | `<config>/zyris-code` | Directory the credential lives in. Set it and it wins outright |
| `ZYRIS_CODE_BG` | — | Paint a page background (`zyris`, or `#rrggbb`). Off by default so the terminal's own background shows; turn it on if wide characters leave smears over SSH |
| `ZYRIS_NODE_TOKEN` | — | Dial with a fixed node token instead of enrolling |
| `ZYRIS_CODE_LOG` | `/tmp/zyris-code.log` | Log file. Logs never go to the terminal — they would land in the middle of the UI |
| `ZYRIS_CODE_WIRE_DEADLINE_SECS` | `55` | Answer the wire before the server gives up on a call; `0` disables it |
| `RUST_LOG` | `zyris_code=info,zyris=warn` | Log filter |
| `NO_COLOR` | — | Suppress colour in the messages printed before the UI starts |

## Building

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

`rustfmt.toml` is checked in and `cargo fmt` is expected to be clean.

## Licence

MIT or Apache-2.0, at your option.
