//! Command-line preflight: `-h/--help`, `-V/--version`, and rejection of
//! unknown arguments. Conventions follow GNU/clap: help and version print
//! to stdout and exit 0; an unexpected argument prints to stderr and
//! exits 2. Everything else (TUI, --headless, --probe-gpu, --fixture)
//! falls through to the existing handlers in main.

pub const VERSION_LINE: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

pub const HELP_TEXT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    " — top-style terminal dashboard for llama.cpp servers and llama-swap routers\n",
    "\n",
    "USAGE:\n",
    "    lsmtop [OPTIONS]\n",
    "\n",
    "OPTIONS:\n",
    "    -h, --help             Print this help\n",
    "    -V, --version          Print version information\n",
    "        --probe-gpu        Print a one-shot GPU backend report and exit\n",
    "        --headless <secs>  Sample without a TUI for <secs> seconds (default 60)\n",
    "        --dump <path>      With --headless: write the timeline as JSON\n",
    "        --target <name>    With --headless: pick a target from config by name\n",
    "        --fixture <dir>    Replay a recorded fixture directory (testing)\n",
    "\n",
    "ENVIRONMENT:\n",
    "    LSMTOP_CONFIG          Path to the config file (default ~/.lsmtop/config.json)\n",
    "\n",
    "User guide: https://github.com/chrismarentette/lsmtop/blob/main/help.md\n",
);

/// Flags that consume the argument after them as a VALUE (so the value is
/// never validated as a flag itself).
const VALUE_FLAGS: [&str; 4] = ["--headless", "--dump", "--target", "--fixture"];

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// No preflight flag matched; continue into normal argument handling.
    Continue,
    /// Print `text` (stdout unless `stream == Stream::Err`) and exit `code`.
    Print { stream: Stream, text: String, code: i32 },
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Stream {
    Out,
    Err,
}

/// Pure argv preflight. `argv` includes argv[0]. HELP/VERSION win over
/// everything (checked before any other argument, clap-style); then any
/// token starting with `-` that is not a known flag is rejected, and any
/// bare positional is rejected too.
pub fn preflight(argv: &[String]) -> Decision {
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    if args.iter().any(|a| *a == "-h" || *a == "--help") {
        return Decision::Print { stream: Stream::Out, text: HELP_TEXT.into(), code: 0 };
    }
    if args.iter().any(|a| *a == "-V" || *a == "--version") {
        return Decision::Print { stream: Stream::Out, text: VERSION_LINE.into(), code: 0 };
    }
    let mut skip_value = false;
    for a in args.iter().skip(1) {
        if skip_value {
            skip_value = false;
            continue;
        }
        if VALUE_FLAGS.contains(a) {
            skip_value = true;
            continue;
        }
        // Known switch-style flags fall through to main's handlers.
        if matches!(*a, "--probe-gpu" | "-h" | "--help" | "-V" | "--version") {
            continue;
        }
        // An lone "-" traditionally means stdin; a TUI takes no input, but
        // rejecting it is the clearer contract. Anything else that looks
        // like a flag or is a bare positional is unexpected.
        let err = format!(
            "error: unexpected argument '{a}' found\n\n\
             For more information try --help"
        );
        return Decision::Print { stream: Stream::Err, text: err, code: 2 };
    }
    Decision::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_flags_print_help_to_stdout_exit_0() {
        for f in ["-h", "--help"] {
            match preflight(&v(&["lsmtop", f])) {
                Decision::Print { stream: Stream::Out, text, code: 0 } => {
                    assert!(text.starts_with("lsmtop "));
                    assert!(text.contains("USAGE:"));
                    assert!(text.contains("--probe-gpu"));
                }
                other => panic!("{f}: {other:?}"),
            }
        }
    }

    #[test]
    fn version_flags_print_name_and_version_exit_0() {
        for f in ["-V", "--version"] {
            match preflight(&v(&["lsmtop", f])) {
                Decision::Print { stream: Stream::Out, text, code: 0 } => {
                    assert_eq!(text, VERSION_LINE);
                    assert!(text.starts_with("lsmtop "));
                    // semver-looking tail, straight from Cargo
                    assert!(text.split_once(' ').unwrap().1.chars().next().unwrap().is_ascii_digit());
                }
                other => panic!("{f}: {other:?}"),
            }
        }
    }

    #[test]
    fn help_wins_over_everything_including_version_and_garbage() {
        assert_eq!(
            preflight(&v(&["lsmtop", "--version", "--help", "--bogus"])),
            Decision::Print { stream: Stream::Out, text: HELP_TEXT.into(), code: 0 }
        );
    }

    #[test]
    fn known_flags_fall_through() {
        for argv in [
            v(&["lsmtop"]),
            v(&["lsmtop", "--probe-gpu"]),
            v(&["lsmtop", "--headless", "5", "--dump", "run.json", "--target", "Server 0"]),
            v(&["lsmtop", "--fixture"]), // missing value: main's default path applies
            v(&["lsmtop", "--fixture", "d"]),
        ] {
            assert_eq!(preflight(&argv), Decision::Continue, "{argv:?}");
        }
    }

    #[test]
    fn unknown_flags_and_positionals_are_exit_2_on_stderr() {
        for argv in [
            v(&["lsmtop", "--frobnicate"]),
            v(&["lsmtop", "-x"]),
            v(&["lsmtop", "somefile"]),
            v(&["lsmtop", "-"]),
            v(&["lsmtop", "--probe-gpu", "--nope"]),
        ] {
            match preflight(&argv) {
                Decision::Print { stream: Stream::Err, text, code: 2 } => {
                    assert!(text.starts_with("error: unexpected argument"));
                    assert!(text.contains("try --help"));
                }
                other => panic!("{argv:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn flag_values_are_never_flagged_as_unknown() {
        // a value that LOOKS odd must survive: --target --weird still
        // consumes it as the value of --target (matches main's leniency)
        assert_eq!(preflight(&v(&["lsmtop", "--headless", "--not-a-number"])), Decision::Continue);
    }
}
