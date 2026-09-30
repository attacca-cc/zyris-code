//! Putting a selection on the system clipboard.
//!
//! **Writing only.** Terminals usually allow *writing* to the clipboard via OSC 52 but block
//! *reading* it (understandably — other apps could snoop), so there is no paste from here: the
//! terminal's own paste (usually `Ctrl+Shift+V` or middle click) arrives as input, in one chunk with
//! bracketed paste on.
//!
//! **Two routes, because neither reaches every setup.** OSC 52 travels with the terminal's own
//! bytes, so it is the one that works over SSH — but only where the terminal accepts it, and tmux
//! drops it from applications by default. A clipboard tool (`wl-copy`, `xclip`, `pbcopy`) works on
//! the machine the app runs on whatever the terminal is — which is exactly the case where the
//! terminal could not be recognised by name. So a local session tries the tool, and OSC 52 goes out
//! wherever the terminal is thought to read it.

use std::io::Write;

use base64::Engine;

/// OSC 52 sequence that puts text on the system clipboard.
pub fn osc52_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// `sequence` wrapped for tmux to pass through to the terminal outside it: a DCS addressed to
/// tmux, with every ESC inside doubled. Read when tmux has `allow-passthrough` on; a tmux that
/// takes OSC 52 itself (`set-clipboard on`) reads the plain one sent beside it.
pub fn tmux_passthrough(sequence: &str) -> String {
    format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
}

/// The clipboard tools to try on this machine, in order, with their arguments.
///
/// **None over SSH.** A tool there would fill the remote machine's clipboard, which nobody at the
/// keyboard can paste from; OSC 52 is the route that reaches them. Windows has no entry: `clip.exe`
/// reads the console code page rather than UTF-8, so Hangul would arrive broken, and Windows
/// Terminal is recognised by name and takes OSC 52.
fn tools(
    env: &dyn Fn(&str) -> Option<String>,
    macos: bool,
) -> Vec<(&'static str, &'static [&'static str])> {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return Vec::new();
    }
    let mut out: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if macos {
        out.push(("pbcopy", &[]));
    }
    if set("WAYLAND_DISPLAY") {
        out.push(("wl-copy", &[]));
    }
    if set("DISPLAY") {
        out.push(("xclip", &["-selection", "clipboard"]));
        out.push(("xsel", &["--clipboard", "--input"]));
    }
    out
}

/// Hands `text` to the first clipboard tool that starts. Written and waited on in a thread of its
/// own, so a slow tool never holds up the screen.
fn through_a_tool(text: &str) -> bool {
    use std::process::{Command, Stdio};
    let env = |k: &str| std::env::var(k).ok();
    for (program, args) in tools(&env, cfg!(target_os = "macos")) {
        let spawned = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { continue };
        let text = text.to_string();
        std::thread::spawn(move || {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
        return true;
    }
    false
}

/// Puts `text` on the system clipboard by every route that can reach it, and says whether any
/// route was tried. `osc52` is `term::Caps::osc52`: whether the terminal is thought to read it.
///
/// A route that was tried can still have been ignored — a terminal that keeps OSC 52 switched off
/// ignores it silently — but `false` is certain: nothing left the app.
pub fn export(text: &str, osc52: bool) -> bool {
    let tool = through_a_tool(text);
    if osc52 {
        let sequence = osc52_sequence(text);
        let mut out = std::io::stdout();
        let _ = out.write_all(sequence.as_bytes());
        if std::env::var_os("TMUX").is_some() {
            let _ = out.write_all(tmux_passthrough(&sequence).as_bytes());
        }
        let _ = out.flush();
    }
    tool || osc52
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OSC 52 takes the form `ESC ] 52 ; c ; <base64> BEL`. Even mixed with Hangul, base64 keeps it safe.
    #[test]
    fn the_osc52_sequence_is_well_formed() {
        let seq = osc52_sequence("한글 test");
        assert!(seq.starts_with("\x1b]52;c;"), "{seq:?}");
        assert!(seq.ends_with('\x07'), "{seq:?}");

        let body = seq.trim_start_matches("\x1b]52;c;").trim_end_matches('\x07');
        let decoded = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "한글 test");
    }

    /// **tmux gets it addressed to itself**, with the inner ESC doubled so tmux does not end the
    /// DCS at it.
    #[test]
    fn the_tmux_wrapping_doubles_every_escape() {
        assert_eq!(
            tmux_passthrough("\x1b]52;c;aGk=\x07"),
            "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string())
    }

    /// **A local desktop gets its own clipboard tool; an SSH session gets none**, since the tool
    /// would fill the remote machine's clipboard.
    #[test]
    fn the_tools_follow_the_desktop_and_stay_off_over_ssh() {
        let names = |pairs, macos| -> Vec<&str> {
            tools(&env(pairs), macos).into_iter().map(|(p, _)| p).collect()
        };
        assert_eq!(names(&[("WAYLAND_DISPLAY", "wayland-1")], false), ["wl-copy"]);
        assert_eq!(names(&[("DISPLAY", ":0")], false), ["xclip", "xsel"]);
        assert_eq!(names(&[], true), ["pbcopy"]);
        assert!(names(&[], false).is_empty(), "no desktop, no tool");
        assert!(names(&[("DISPLAY", ":0"), ("SSH_CONNECTION", "a b c d")], false).is_empty());
        assert!(names(&[("SSH_TTY", "/dev/pts/1")], true).is_empty());
    }
}
