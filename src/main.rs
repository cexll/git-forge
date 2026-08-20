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
            eprintln!("git-forge: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_forge(argv: &[String]) -> Result<String, String> {
    match argv.first().map(|s| s.as_str()) {
        // `git forge issue ...` → git runs `git-forge` with argv = ["issue", ...]
        Some("issue") => git_forge::cli::run_issue(&argv[1..]),
        // `git forge pr ...` → argv = ["pr", ...]
        Some("pr") => git_forge::cli::run_pr(&argv[1..]),
        // Direct binary form `git-forge forge issue ...` (used in tests).
        Some("forge") => match argv.get(1).map(|s| s.as_str()) {
            Some("issue") => git_forge::cli::run_issue(&argv[2..]),
            Some("pr") => git_forge::cli::run_pr(&argv[2..]),
            Some("help") | Some("-h") | Some("--help") => Ok(top_help()),
            Some(other) => Err(format!("unknown forge subcommand '{other}'")),
            None => Ok("usage: git forge <issue|pr> ...".to_string()),
        },
        // `git-forge --help` (verify-cli contract): exit 0 with usage.
        Some("help") | Some("-h") | Some("--help") => Ok(top_help()),
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Ok("usage: git forge <issue|pr> ...".to_string()),
    }
}

fn top_help() -> String {
    "usage: git forge <issue|pr> ...\n\
     \nsubcommands:\n\
     \x20 issue  new|list|show|comment|close|reopen\n\
     \x20 pr     create|list|show|comment|review|diff|merge\n\
     \x20 run `git forge issue --help` / `git forge pr --help` for per-subcommand usage"
        .to_string()
}

fn run_wrapper(argv: &[String]) -> Result<String, String> {
    let invoked_as_pr = std::env::args()
        .next()
        .map(|a| a.rsplit('/').next().unwrap_or(a.as_str()) == "git-pr")
        .unwrap_or(false);
    if invoked_as_pr {
        git_forge::cli::run_pr(argv)
    } else {
        git_forge::cli::run_issue(argv)
    }
}
