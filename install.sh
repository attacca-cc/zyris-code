#!/bin/sh
# Install zyris-code on macOS or Linux.
#
#   curl -fsSL https://github.com/attacca-cc/zyris-code/releases/latest/download/install.sh | sh
#
# Options (after `| sh -s --`):
#   --version <tag>     install that release instead of the newest
#   --dir <path>        install there instead of ~/.local/bin
#   --no-modify-path    do not touch any shell startup file
#
# **Nothing here needs root.** It installs under your home directory, so a machine you only have
# an account on is enough.
set -eu

REPO="attacca-cc/zyris-code"
BIN="zyris-code"
# **`zyris` is not ours.** This installer used to drop a `zyris` symlink beside the binary as a
# short name. On Linux the command that installs a Zyris node *is* `zyris`, so squatting it here
# means whichever of the two was installed second silently wins — and the loser is a command
# somebody has already learned. The name is left alone; a symlink an earlier version of this
# script created is removed below.
STALE_ALIAS="zyris"

VERSION=""
INSTALL_DIR="${ZYRIS_CODE_INSTALL_DIR:-$HOME/.local/bin}"
MODIFY_PATH=1

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a tag}"; shift 2 ;;
    --dir) INSTALL_DIR="${2:?--dir needs a path}"; shift 2 ;;
    --no-modify-path) MODIFY_PATH=0; shift ;;
    -h|--help)
      say "install zyris-code"
      say "  --version <tag>     install that release instead of the newest"
      say "  --dir <path>        install there instead of ~/.local/bin"
      say "  --no-modify-path    do not touch any shell startup file"
      exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

# ── Which build ──────────────────────────────────────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux) os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) die "unsupported system: $os. Build from source: cargo install --git https://github.com/$REPO zyris-code" ;;
esac
case "$arch" in
  x86_64|amd64) arch_part="x86_64" ;;
  arm64|aarch64) arch_part="aarch64" ;;
  *) die "unsupported architecture: $arch" ;;
esac
target="${arch_part}-${os_part}"
archive="${BIN}-${target}.tar.gz"

if [ -n "$VERSION" ]; then
  base="https://github.com/$REPO/releases/download/$VERSION"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

# ── Fetch ────────────────────────────────────────────────────────────────────
#
# Two ways of fetching, on purpose. `fetch` is silent and `show` draws a progress bar, and only the
# archive gets the bar: it is the one download here big enough for the wait to need explaining, and
# a bar for a one-line checksum file is noise.
#
# **The bar is only drawn for somebody watching.** Written to something that is not a terminal,
# curl prints a line per update — thousands of them in a build log. `[ -t 2 ]` asks whether stderr
# is a terminal, which is also where the bar goes, and that is why it survives `curl … | sh`: the
# script has the pipe on stdin, not on stderr.
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
  if [ -t 2 ]; then
    show() { curl -#fSL "$1" -o "$2"; }
  else
    show() { fetch "$1" "$2"; }
  fi
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
  # **Asked, not assumed.** `--show-progress` arrived in wget 1.16 and busybox's wget has never had
  # it; passing it blind would turn a download into "unrecognized option" on the machines least
  # likely to have a second way to get the file.
  if [ -t 2 ] && wget --help 2>&1 | grep -q -- --show-progress; then
    show() { wget -q --show-progress -O "$2" "$1"; }
  else
    show() { fetch "$1" "$2"; }
  fi
else
  die "neither curl nor wget is available"
fi

tmp="$(mktemp -d)"
# However this ends, do not leave a directory behind.
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive"
show "$base/$archive" "$tmp/$archive" \
  || die "no build for $target in that release. See https://github.com/$REPO/releases"

# **Check the download before unpacking it.** This is a binary about to go on your PATH.
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then
    sum="$(sha256sum "$tmp/$archive" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    sum="$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)"
  else
    sum=""
    say "warning: no sha256 tool found, skipping the checksum"
  fi
  if [ -n "$sum" ]; then
    want="$(grep " $archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1 || true)"
    [ -n "$want" ] || die "$archive is not listed in SHA256SUMS"
    [ "$sum" = "$want" ] || die "checksum mismatch for $archive — refusing to install"
    say "checksum ok"
  fi
else
  say "warning: SHA256SUMS is missing from this release, skipping the checksum"
fi

# ── Install ──────────────────────────────────────────────────────────────────
tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/$BIN" ] || die "the archive did not contain $BIN"

mkdir -p "$INSTALL_DIR"
# Write beside the target and rename, so a half-written file never sits on PATH — and so
# replacing a copy that is currently running does not fail.
install_tmp="$INSTALL_DIR/.$BIN.new.$$"
cp "$tmp/$BIN" "$install_tmp"
chmod 755 "$install_tmp"
mv -f "$install_tmp" "$INSTALL_DIR/$BIN"

# Take back the `zyris` name if an earlier version of this script left it here.
#
# **Only a symlink pointing at our own binary.** That is the shape this script made, so it is the
# one we can be sure we own. Where `ln -s` failed the old fallback wrote a *copy*, which is
# indistinguishable from a file somebody put there on purpose — that one is reported rather than
# deleted, because guessing wrong deletes a stranger's program.
stale="$INSTALL_DIR/$STALE_ALIAS"
if [ -L "$stale" ] && [ "$(readlink "$stale")" = "$BIN" ]; then
  rm -f "$stale"
  say "removed the old $STALE_ALIAS link; the command is $BIN"
elif [ -e "$stale" ] && [ ! -L "$stale" ] && cmp -s "$stale" "$INSTALL_DIR/$BIN"; then
  say "note: $stale is a copy of $BIN left by an older installer — remove it if you want the"
  say "      $STALE_ALIAS name free for a Zyris node"
fi

# **Do not run it to ask its version.** It is a TUI that starts on launch and takes no
# arguments, so that would open the app in the middle of an install.
say "installed $BIN to $INSTALL_DIR"

# ── shell integration ────────────────────────────────────────────────────────
# Up to two lines get written, **each with its own marker**, so a later version adds whatever an
# earlier one did not: the PATH export, and — zsh only — an alias that stops the shell from eating
# a prompt before this program ever starts.
#
# **zsh refuses to run a command carrying a glob that matched nothing.** `zyris-code -p 이거 뭐야?`
# dies as `no matches found: 뭐야?` and the binary is never started, so nothing inside it can
# help — the fix has to live in the shell. `noglob` turns matching off for this one command.
# bash needs none of it (an unmatched pattern is passed through as text), and fish has no
# equivalent, so there quoting stays the only way.
# The PATH marker keeps its old wording: installs made by earlier versions carry it, and changing
# it would make this add a second copy of a line that is already there.
path_mark="# added by zyris-code installer"
glob_mark="# zyris-code installer: keeps ? and * in a prompt"

# A marker per line written, so a second run finds its own work instead of appending again.
# **The markers must not be prefixes of one another** — `grep -F` would then find the wrong one
# and decide the work was already done.
add_to() {
  file="$1"
  mark="$2"
  text="$3"
  [ -e "$file" ] || : > "$file"
  if grep -Fq "$mark" "$file" 2>/dev/null; then
    return 1
  fi
  printf '\n%s\n%s\n' "$mark" "$text" >> "$file"
  return 0
}

# Already reachable? Then the PATH line is not wanted — writing it anyway is how startup files
# collect the same line four times. **The alias may still be missing**, so this no longer leaves.
case ":${PATH}:" in
  *":$INSTALL_DIR:"*) on_path=1 ;;
  *) on_path=0 ;;
esac

if [ "$MODIFY_PATH" = 0 ]; then
  # **An update passes this**, and an update has no business rewriting startup files.
  say ""
  if [ "$on_path" = 0 ]; then
    say "$INSTALL_DIR is not on your PATH. Add it yourself:"
    say "    export PATH=\"$INSTALL_DIR:\$PATH\""
    say ""
  fi
  say "Run it with:  $BIN"
  exit 0
fi

shell_name="$(basename "${SHELL:-sh}")"
path_line="export PATH=\"$INSTALL_DIR:\$PATH\""
# One name now. An install made by an earlier version carries both, and that line is left as it
# is: `add_to` skips a file that already has the marker, and `noglob zyris` does no harm to a
# `zyris` that turns out to be a node.
glob_line="alias $BIN='noglob $BIN'"

edited=""
aliased=""
case "$shell_name" in
  bash)
    # macOS terminals start a login shell, which reads .bash_profile and not .bashrc; Linux
    # terminals do the opposite. Writing to whichever exists covers both without guessing.
    if [ "$on_path" = 0 ]; then
      for f in "$HOME/.bashrc" "$HOME/.bash_profile"; do
        [ -e "$f" ] || [ "$f" = "$HOME/.bashrc" ] || continue
        if add_to "$f" "$path_mark" "$path_line"; then edited="$edited $f"; fi
      done
    fi
    ;;
  zsh)
    rc="${ZDOTDIR:-$HOME}/.zshrc"
    if [ "$on_path" = 0 ] && add_to "$rc" "$path_mark" "$path_line"; then edited="$edited $rc"; fi
    if add_to "$rc" "$glob_mark" "$glob_line"; then aliased="$rc"; fi
    ;;
  fish)
    # fish has no `export`, and conf.d is the place meant for exactly this.
    if [ "$on_path" = 0 ]; then
      conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/zyris-code.fish"
      mkdir -p "$(dirname "$conf")"
      if add_to "$conf" "$path_mark" "fish_add_path \"$INSTALL_DIR\""; then edited="$edited $conf"; fi
    fi
    ;;
  *)
    if [ "$on_path" = 0 ] && add_to "$HOME/.profile" "$path_mark" "$path_line"; then
      edited="$edited $HOME/.profile"
    fi
    ;;
esac

say ""
if [ -n "$edited" ]; then
  say "Added $INSTALL_DIR to your PATH in:$edited"
  say "Open a new terminal, or run this once in this one:"
  say "    export PATH=\"$INSTALL_DIR:\$PATH\""
  say ""
elif [ "$on_path" = 0 ]; then
  say "$INSTALL_DIR is already set up in your shell's startup file."
  say "Open a new terminal to pick it up."
  say ""
fi
if [ -n "$aliased" ]; then
  say "Taught zsh to leave your prompts alone, in $aliased — so this works unquoted:"
  say "    $BIN -p what is broken here?"
  say ""
fi
say "Then run:  $BIN"
