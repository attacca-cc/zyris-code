# zyris-code

[![release](https://img.shields.io/github/v/release/attacca-cc/zyris-code?label=release&color=blue)](https://github.com/attacca-cc/zyris-code/releases/latest)
[![check](https://github.com/attacca-cc/zyris-code/actions/workflows/check.yml/badge.svg)](https://github.com/attacca-cc/zyris-code/actions/workflows/check.yml)
[![licence](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)](#licence)

A terminal coding client for [Attacca](https://attacca.cc) agents, built with
[ratatui](https://ratatui.rs) and connected over the
[Zyris](https://github.com/attacca-cc/zyris) protocol.
Introduction and screenshots: **[zyris.attacca.cc/zyris-code](https://zyris.attacca.cc/zyris-code)**.

It is not just a chat window. **It hands your machine to the agent**: reading and
editing files, searching the tree, running shell commands, and exposing the tools
of any local MCP server you configure. The agent runs on Attacca; the tools run
here.

> The interface is bilingual (Korean and English), switchable in `/config` — English is the
> default, and Korean follows the locale or your choice. Mode names and prompts are given
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

## Installing

**macOS / Linux**

```sh
curl -fsSL https://github.com/attacca-cc/zyris-code/releases/latest/download/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://github.com/attacca-cc/zyris-code/releases/latest/download/install.ps1 | iex
```

Both install under your own account and need no administrator rights —
`~/.local/bin` on macOS and Linux, `%LOCALAPPDATA%\Programs\zyris-code` on
Windows — and put that directory on your PATH for bash, zsh, fish or PowerShell.
Open a new terminal and the command is:

```sh
zyris-code
```

**There used to be a short `zyris` alias, and it is gone.** `zyris` is the command
that installs a Zyris node, and two programs answering to one name means whichever
was installed second silently wins. An installer that placed the old link removes it
on the next run; a `zyris` it did not place is left alone and mentioned. Set
`ZYRIS_CODE_INSTALL_DIR` to install elsewhere, `--no-modify-path`
(`-NoModifyPath` on Windows) to leave your shell startup files alone, and
`--version <tag>` to pin a release. Each archive is listed in the release's
`SHA256SUMS`, which the installers check before unpacking anything.

To build it yourself instead:

```sh
cargo install --git https://github.com/attacca-cc/zyris-code zyris-code
```

## Running

```bash
zyris          # or, from a checkout: cargo run -p zyris-code
```

On first launch an 8-digit enrollment code is shown in a panel with the URL to
open. Enter it on any device with a browser, pick (or create) the system this
machine is in Attacca, and approve. That issues one credential for the program
`zyris-code` on that system; zyris-code keeps it in
`~/.config/zyris-code/wss-<server>-<profile>.json` and does not need a browser
again unless it is revoked. A credential file left by a build from before
credentials existed is discarded, and the enrollment panel appears once.

Approve **every** scope on that screen. Scopes are fixed at approval time, so a
grant missing `agents:read` or `projects:read` shows an empty agent or project
list rather than an error. zyris-code names what is missing and asks once for a
fresh code.

**Each window is a node of its own**, named after the directory it was started
in. Attacca shows it as `<system>/zyris-code/<directory>` — for example
`laptop/zyris-code/myrepo` — and a second window in the same directory becomes
`myrepo-2` for as long as both are open. Override the last part with
`ZYRIS_NODE_NAME`; `/cwd` shows the path this window was given. The agent sees
every node's tools under one name and picks the computer with each tool's
`node_path` argument; the session preamble and the `rules` tool tell it which
path is the one you are sitting at. **The directory-access policy and the
plan/edit mode that judge a call belong to the window that received it.**

If the credential is ever revoked — in Attacca's settings, directly or by
deleting the system — zyris-code notices at the next connection and **draws the
enrollment code in the UI itself**: a panel over the conversation with the URL,
the code and a countdown. The panel is dismissed only with `Esc`; approving in the
browser closes it on its own, and a fresh code re-opens it if the old one lapses.
The same panel is what you see on a first launch, since the UI starts before the
connection is even established.

Run it **from the directory you want to work in** — that directory defines the
fence described below.

## The working directory is a fence

Everything inside the directory you launched from runs without interruption. For
anything that reaches outside it — reads included — **one setting decides**
(`/config`): `deny` (the default) refuses the call with a message, `allow` runs
it without asking.

- **Reads count too.** The point is to keep the agent from wandering across your
  whole disk, so the policy covers them as well.
- **The policy is persistent.** `deny` is the default; `/config dir allow` opens
  the whole disk to tools, `/config dir deny` closes it again. There is no
  per-directory asking anymore — that window was removed because it only broke
  the flow. `/config` shows the current value.
- **A shell command cannot be fenced completely.** `terminal.exec` runs an
  arbitrary program, and no amount of reading the command text tells you where it
  will go — `sh -c` alone can do anything. Absolute paths and `../` in the command
  are caught, which stops accidental escapes, but treat it as a net rather than a
  wall. Symlinks pointing outward are not followed either.

## Modes

`Shift+Tab` cycles through four modes, shown in the bottom bar. A mode decides two
things: whether tools may run, and where your next message goes.

| Mode | Tools | Your next message |
|---|---|---|
| **normal** (일반) | Run. The fence above still applies. | Goes to the conversation you are already having. |
| **plan** (계획) | Nothing is changed and no command runs; the agent has to describe its plan first. Reading still works — you cannot plan what you cannot see. | Same as normal. |
| **work** (일) | Run, same as normal. | Opens an Attacca **work**: it is planned into a task graph, each task running in its own git worktree. Two gates need a person to open them. |
| **job** (작업) | Run, same as normal. | Opens an Attacca **job** — hand it over and it runs to the end. If it asks something back, answer right here. |

`normal` and `plan` share one conversation, so you can switch between them mid-thread
without losing your place. `work` and `job` open something new with your **next message
only**; after that you are talking to it.

`/mode normal|plan|work|job` does the same thing without the keyboard.

Whatever you open lands in **the project you last picked** (`←`), not the default one.
The `+ New project` row in the project list opens a small form — type a name and an
optional description, Enter creates it and moves you in.

`work` and `job` are deliberately untranslated — they are what Attacca calls them, so
what you open here is what you look up there.

## What the agent gets

| Capability | Tools |
|---|---|
| `search` | `glob`, `grep` — respects `.gitignore`, skips binaries |
| `file_io` | `stat`, `list`, `read`, `read_stream` — **read only** |
| `code_edit` | `edit`, `write`, `version` |
| `terminal` | `exec`, plus a full PTY: `open`, `read`, `write`, `screen`, `resize`, `close` |
| `skill` | `list`, `load` |
| `rules` | `load` — the current `CLAUDE.md`·`AGENTS.md`, read now rather than as the session was created |
| `wait` | `start`, `until`, `logs`, `list`, `stop` — background commands, and waiting on a local build, a remote build or a work |
| `git` | `status`, `log`, `diff`, `branches`, `switch`, `commit`, `push`, plus GitHub: `issues`, `pulls`, `comment`, `create_issue`, `create_pull`, `review`, `request_review` |
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

Type `/` and the command list opens; it narrows as you type. Commands run locally;
only `/agent` and `/account` ask the server, for exactly what they need.

**`/help` is the list**, and it prints the keys too. There is deliberately no copy of
either here: a table in a README drifts from the program one release at a time, and
each page goes on looking right on its own. The list that ships with your build is the
one that is true for your build.

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

`/help` prints the bindings your build actually has. What follows is only the handful
whose behaviour is worth explaining rather than listing.

`Esc` stops a running turn — it is the only key that does, and the card it was
cut in then reads `Stopped` rather than `Done`. `Ctrl+C` does not cancel:
pressing it arms quitting, and the next press quits.
**The second press arms quitting even while a turn is still running**, so a server that
has stopped answering cannot trap you in the window.

**Closing the window stops the turn on the server.** The turn runs there, not
here — left alone it keeps thinking, fails every tool call looking for a node
that is gone, and spends credit doing it. `SIGTERM` and `SIGHUP` (closing the
terminal) take the same path. If the server does not answer within three
seconds the window closes anyway.

**A work card is one stretch of working — everything the agent thought and did
between two things it said to you.** Its head keeps rewriting itself to say what
is happening ("retrying the node" → "writing the report"), and reads `Done` once
the stretch is over — or `Stopped` when you ended it with `Esc`, since a run
somebody cut is not one that finished. What the agent says to you stands outside
the card, so a folded card never hides an answer.

A running card is open; a finished one folds itself into that one line. `Ctrl+O`
folds and unfolds the latest card, and from then on that card is yours — it stops
following the turn. Inside the card, the reasoning chips stay folded until you
click one: watching the model talk itself round is noise, and it pushes the tool
rows off the screen. Clicking a tool row opens its input and output, rendered per
tool rather than as raw JSON.

Switching to another session (`←`) while a turn is running does not stop that
turn — it keeps running on the server, and its events stop appearing here. When
you switch back, the conversation is re-read and you see where it got to.

Messages typed while a turn is running are queued and sent in order when it ends.

## Environment

| Variable | Default | Does |
|---|---|---|
| `ZYRIS_CODE_AGENT` | `Main Agent` | Agent to connect to at startup |
| `ZYRIS_NODE_NAME` | the working directory's name | The node name this window asks for; Attacca adds `-2` while another window of this credential holds it |
| `ZYRIS_PROFILE` | `zyris-code` | Credential file within that directory, so one machine can hold several identities |
| `ZYRIS_CONFIG_DIR` | `<config>/zyris-code` | Directory the credential lives in. Set it and it wins outright |
| `ZYRIS_CODE_BG` | — | Paint a page background (`zyris`, or `#rrggbb`). Off by default so the terminal's own background shows; turn it on if wide characters leave smears over SSH |
| `ZYRIS_CREDENTIAL` / `ZYRIS_CREDENTIAL_FILE` | — | Dial with a `zc_` credential issued in Attacca (or a file holding one) instead of enrolling |
| `ZYRIS_CODE_LOG` | `/tmp/zyris-code.log` | Log file. Logs never go to the terminal — they would land in the middle of the UI |
| `ZYRIS_CODE_EXEC_MAX_SECS` | `1800` | Longest a `terminal.exec` command may run before this node kills it — and, because the two must agree, the wait it asks callers for. `0` lifts the ceiling, and then only the agent's own `timeout_ms` bounds a command |
| `ZYRIS_CODE_WIRE_DEADLINE_SECS` | `55` | Answer the wire before the server gives up on a call, for the tools that declare no limit of their own (`wait.until`); `0` disables it |
| `ZYRIS_CODE_MOUSE` | on | `0` hands the mouse back to the terminal, so copy-on-select and the scrollback drag work as they do everywhere else. Click-to-fold, drag-to-copy and Ctrl+click go with it |
| `ZYRIS_CODE_HYPERLINKS` | detected | Force OSC 8 link markup on or off. Only terminals known to read it are sent any, because one that does not prints the escape bytes across the screen. Links stay Ctrl+clickable either way — the app opens them itself |
| `ZYRIS_CODE_OSC52` | detected | Force system-clipboard writes on or off. Terminals differ, and several that draw links keep clipboard writes switched off until told otherwise |
| `RUST_LOG` | `zyris_code=info,zyris=warn` | Log filter |
| `NO_COLOR` | — | Suppress colour in the messages printed before the UI starts |

## Updating itself

zyris-code asks GitHub once per launch — **before the screen opens** — whether there is a newer
release. What happens then is `/config`'s `update` setting:

| | |
|---|---|
| `auto` (default) | Installs it there and then, on the terminal you started it from, and comes back on the new version in that same window. |
| `notify` | Says a newer release exists; `/update` installs it. |
| `off` | Never looks. |

`zyris-code --update` does the same thing without opening anything, whatever the setting says. Either
way the download draws a progress bar, and the installer's own account of what it is doing goes to
your terminal rather than being swallowed.

**Nothing you have is touched by an update.** The installers only write to the directory the
binary lives in. Credentials, settings, language, the GitHub token, plugins, skills and the undo
log all live under `~/.config/zyris-code/` and `~/.cache/zyris-code/`
(`%LOCALAPPDATA%` on Windows), which no installer reads or removes — a reinstall picks up exactly
where you left off, still enrolled.

**The release's own installer does the replacing**, fetched from the release being installed and
given that tag. It is the script tested against that build, and pinning the tag means `latest`
moving between the check and the install cannot change what lands. Checksums are verified before
anything is unpacked, as with a first install.

**The update keeps the terminal it was started in.** On unix the process replaces itself with the
new version; on Windows, which has no way to do that, it starts the new one and waits for it. A
running `.exe` cannot be overwritten there at all, so the installer renames the old one aside
first — which is why an update needs no manual step on Windows either.

## Print mode

```bash
zyris-code -p what does this repo do   # one turn, the answer on stdout, exit
cat notes.md | zyris-code -p           # the prompt from stdin
zyris-code -p "..." > answer.md        # only the answer is printed, so it pipes
```

**No quotes needed** — everything after the flag is the prompt.

The exception is zsh, and it is not this program's doing: a prompt containing `?` or `*` is read
as a filename pattern, and when nothing matches, zsh refuses to run the command at all. The binary
is never started, so it cannot help. `install.sh` therefore leaves an alias in `.zshrc` that runs
the command under `noglob`, after which `zyris-code -p what broke here?` works as typed. Without
that alias, quote the prompt. bash needs none of this, and PowerShell passes arguments through
untouched.

An install made before the `zyris` name was given up carries that alias for both names. It is left
as it is: the installer skips a startup file that already has its marker, and `noglob zyris` costs
nothing if `zyris` later turns out to be a node.

**Print mode still hands this computer over.** The node announces the same capabilities as the
screen does, so the agent reads and changes files here and runs commands — `/config`'s `dir`
setting governs it exactly as it does interactively. It is not a hosted question-and-answer.

Only the agent's answer reaches stdout. Tool calls, reasoning and status are not printed, and logs
go to a file as always.

## Building

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

`rustfmt.toml` is checked in and `cargo fmt` is expected to be clean.

`tests/pty.rs` runs the app on a real pseudo-terminal — a unix pty on Linux and macOS, a ConPTY on
Windows 10 1809 and later — so drawing, keystrokes and shutdown are covered on every platform by
`cargo test`. It talks to no network. On Windows, `scripts\windows_check.bat` fetches this branch,
runs all of the above, and writes a report with a checklist for the parts that need a person.

## Contributing

**Write English.** Code comments, doc comments, test names, commit messages, pull request
titles and bodies, and issues are all in English. Contributors read them, and anything else
shuts most of them out. `panic!`, `expect` and log messages count as writing too — they are
read by whoever is debugging.

**The one exception is `lang.rs`.** The interface is bilingual and switches in `/config`, so
the Korean side of `lang.rs` is a feature, not a leftover — don't translate it away. User-facing
text belongs there rather than hardcoded at the call site, which is also how it gets an English
version at all.

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org)
(`type(scope)!: description`). A `commit-msg` hook installed by `cargo-husky` on the first
`cargo test` enforces the subject line; `CARGO_HUSKY_DONT_INSTALL_HOOKS=true` skips installing it.

## Licence

MIT or Apache-2.0, at your option.
