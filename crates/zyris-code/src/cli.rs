//! What the command line asked for.
//!
//! **Pure, and that is the point.** Deciding what to run has to be testable without a terminal, a
//! connection or an account — `main` does the doing, this only says what.
//!
//! The binary answers to two names: `zyris-code` from `cargo install`, and `zyris` from the
//! symlink `install.sh` lays down beside it. They are the same file, so `-p` works under either.

/// What to do with this invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// Open the screen. What this app has always done, and still what happens with no arguments.
    Screen,
    /// One turn, no screen: send this and print what comes back.
    ///
    /// `None` means the prompt was not on the command line and has to be read from stdin, so
    /// `zyris -p < notes.md` and `… | zyris -p` work — a prompt worth piping in is usually one too
    /// long to want in shell quoting.
    Print(Option<String>),
    Help,
    Version,
    /// Something we could not read. Held rather than printed here, so the one place that writes to
    /// the shell stays in `main`.
    Bad(String),
}

/// Reads the arguments, without the program name.
///
/// **An unknown flag is refused rather than ignored.** A misspelled `--pirnt` that quietly opened
/// the screen instead would look like the app hanging, and in a script it would hang for real.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Run {
    let args: Vec<String> = args.into_iter().collect();
    // **The first argument settles it.** There is nothing here that combines with anything else —
    // `-p` swallows the whole tail as the prompt, and the other two answer and leave — so walking
    // further would only invent orderings nobody asked for.
    let Some(first) = args.first().map(String::as_str) else {
        return Run::Screen;
    };
    let prompt =
        |text: &str| Run::Print((!text.trim().is_empty()).then(|| text.trim().to_string()));
    match first {
        "-h" | "--help" => Run::Help,
        "-V" | "--version" => Run::Version,
        // Everything after the flag is the prompt, joined back with the spaces the shell split on.
        // Taking one word would silently drop the rest of an unquoted sentence — a shorter
        // question, answered, with nothing to show it had been cut.
        "-p" | "--print" => prompt(&args[1..].join(" ")),
        // `--print=…` in one piece, the way long options are often written.
        _ if first.starts_with("--print=") => prompt(first.trim_start_matches("--print=")),
        // A bare word is not a prompt. `claude` takes one, but this app's argument-free behaviour
        // is the screen, and guessing would make `zyris typo` open a turn and spend credit on it.
        _ => Run::Bad(first.to_string()),
    }
}

/// What `--help` prints. Kept next to the parser so a flag cannot be added without a line here.
pub fn help(program: &str) -> String {
    format!(
        "\
{program} — talk to an Attacca agent from the terminal, and hand it this computer.

USAGE
    {program}                 open the screen
    {program} -p <prompt>     run one turn, print the answer, exit
    … | {program} -p          take the prompt from stdin

OPTIONS
    -p, --print <prompt>  Print mode. Only the agent's answer goes to stdout, so it pipes.
    -h, --help            This.
    -V, --version         Version.

Print mode still hands this computer over: the agent can read and change files here and
run commands, exactly as it can with the screen open.

Environment variables are listed in the README.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(line: &str) -> Run {
        parse(line.split_whitespace().map(str::to_string))
    }

    #[test]
    fn no_arguments_opens_the_screen() {
        assert_eq!(parse_of(""), Run::Screen);
    }

    /// **The whole tail is the prompt.** Taking one word would drop the rest of a sentence the
    /// shell split for us, and the loss would be invisible — a shorter question, answered.
    #[test]
    fn the_prompt_is_everything_after_the_flag() {
        assert_eq!(
            parse_of("-p what does this repo do"),
            Run::Print(Some("what does this repo do".into()))
        );
        assert_eq!(parse_of("--print hello"), Run::Print(Some("hello".into())));
        assert_eq!(parse_of("--print=hello"), Run::Print(Some("hello".into())));
    }

    /// A prompt worth piping in is usually one too long to want inside shell quoting.
    #[test]
    fn a_missing_prompt_means_read_it_from_stdin() {
        assert_eq!(parse_of("-p"), Run::Print(None));
        assert_eq!(parse_of("--print   "), Run::Print(None));
        assert_eq!(parse_of("--print="), Run::Print(None));
    }

    /// **A flag that starts with `-` inside the prompt still belongs to the prompt.** Otherwise
    /// asking about a command-line switch would be unanswerable.
    #[test]
    fn flags_after_the_prompt_flag_are_part_of_the_prompt() {
        assert_eq!(
            parse_of("-p explain --release to me"),
            Run::Print(Some("explain --release to me".into()))
        );
    }

    #[test]
    fn help_and_version_are_answered() {
        for line in ["-h", "--help"] {
            assert_eq!(parse_of(line), Run::Help);
        }
        for line in ["-V", "--version"] {
            assert_eq!(parse_of(line), Run::Version);
        }
    }

    /// **What we cannot read is refused, not ignored.** A misspelled flag that opened the screen
    /// instead would look like a hang, and inside a script it would be one.
    #[test]
    fn something_unreadable_is_refused_rather_than_ignored() {
        assert_eq!(parse_of("--pirnt hello"), Run::Bad("--pirnt".into()));
        assert_eq!(parse_of("-x"), Run::Bad("-x".into()));
        // A bare word is not taken as a prompt: `zyris typo` must not spend credit on a turn.
        assert_eq!(parse_of("typo"), Run::Bad("typo".into()));
    }

    /// Help names every flag the parser answers to, so one cannot be added without saying so.
    #[test]
    fn every_flag_is_in_the_help() {
        let text = help("zyris");
        for flag in ["-p", "--print", "-h", "--help", "-V", "--version"] {
            assert!(text.contains(flag), "{flag} is not in the help");
        }
    }
}
