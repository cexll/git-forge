use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // Thin wrapper dispatch: when invoked via `git-issue` or `git-pr` on PATH,
    // argv[0] is the alias and the first token is the subcommand, not a `forge`
    // token.
    let invoked_as_wrapper = std::env::args()
        .next()
        .map(|a| {
            let base = a.rsplit('/').next().unwrap_or(&a).to_string();
            base == "git-issue" || base == "git-pr"
        })
        .unwrap_or(false);

    let result = if invoked_as_wrapper {
        run_wrapper(&argv)
    } else {
        run_forge(&argv)
    };

    match result {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            if argv.iter().any(|arg| arg == "--json") {
                println!("{}", err.to_json());
            }
            eprintln!("git-forge: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_forge(argv: &[String]) -> Result<String, git_forge::cli_error::CliError> {
    match argv.first().map(|s| s.as_str()) {
        // `git forge issue ...` → git runs `git-forge` with argv = ["issue", ...]
        Some("issue") => git_forge::issue::run_issue(&argv[1..]),
        // `git forge pr ...` → argv = ["pr", ...]
        Some("pr") => git_forge::cli::run_pr(&argv[1..]),
        // `git forge ci ...` → argv = ["ci", ...]
        Some("ci") => git_forge::cli::run_ci(&argv[1..]),
        // `git forge version` (#7): one bare semver line, exit 0 — the
        // adapter's version-gate probe.
        Some("version") => git_forge::machine::cmd_version(&argv[1..]),
        // Direct binary form `git-forge forge issue ...` (used in tests).
        Some("forge") => match argv.get(1).map(|s| s.as_str()) {
            Some("issue") => git_forge::issue::run_issue(&argv[2..]),
            Some("pr") => git_forge::cli::run_pr(&argv[2..]),
            Some("ci") => git_forge::cli::run_ci(&argv[2..]),
            Some("version") => git_forge::machine::cmd_version(&argv[2..]),
            Some("help") | Some("-h") | Some("--help") => Ok(top_help()),
            Some(other) => Err(git_forge::cli_error::CliError::new(
                "E_USAGE",
                format!("unknown forge subcommand '{other}'"),
                other,
                "Run git forge help.",
                false,
            )),
            None => Ok(top_help()),
        },
        // `git-forge --help` (verify-cli contract): exit 0 with usage.
        Some("help") | Some("-h") | Some("--help") => Ok(top_help()),
        Some(other) => Err(git_forge::cli_error::CliError::new(
            "E_USAGE",
            format!("unknown command '{other}'"),
            other,
            "Run git forge help.",
            false,
        )),
        None => Ok(top_help()),
    }
}

fn top_help() -> String {
    "usage: git forge <issue|pr|ci> ...\n\
     \nsubcommands:\n\
     \x20 issue  apply|new|list|show|comment|close|reopen\n\
     \x20 pr     create|list|show|comment|review|diff|merge\n\
     \x20 ci     run <n>\n\
     \x20 version\n\
     \x20 run `git forge issue --help` / `git forge pr --help` / `git forge ci --help` for per-subcommand usage\n\
     \nKnown quirk: bare `git forge --help` exits 1 — git intercepts a leading\n\
     \x20`--help` and looks up a man page that does not exist for custom\n\
     \x20subcommands. Use `git forge help` (this text) instead."
        .to_string()
}

fn run_wrapper(argv: &[String]) -> Result<String, git_forge::cli_error::CliError> {
    let invoked_as_pr = std::env::args()
        .next()
        .map(|a| a.rsplit('/').next().unwrap_or(a.as_str()) == "git-pr")
        .unwrap_or(false);
    if invoked_as_pr {
        git_forge::cli::run_pr(argv)
    } else {
        git_forge::issue::run_issue(argv)
    }
}
