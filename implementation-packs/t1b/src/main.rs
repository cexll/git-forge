use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // Thin `git-issue` wrapper: when invoked via that alias (argv[0] resolves
    // to `git-issue`), the first token is the issue subcommand, not `forge
    // issue`. We detect the alias from the program name.
    let invoked_as_wrapper = std::env::args()
        .next()
        .map(|a| {
            let base = a.rsplit('/').next().unwrap_or(&a).to_string();
            base == "git-issue" || base.starts_with("git-issue")
        })
        .unwrap_or(false);

    let result = if invoked_as_wrapper {
        // git-issue <sub> ...  ->  forge issue <sub> ...
        run_issue_wrapper(&argv)
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
        // Direct binary form `git-forge forge issue ...` (used in tests).
        Some("forge") => match argv.get(1).map(|s| s.as_str()) {
            Some("issue") => git_forge::cli::run_issue(&argv[2..]),
            Some(other) => Err(format!("unknown forge subcommand '{other}'")),
            None => Ok("usage: git forge <issue> ...".to_string()),
        },
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Ok("usage: git forge <issue> ...".to_string()),
    }
}

fn run_issue_wrapper(argv: &[String]) -> Result<String, String> {
    git_forge::cli::run_issue(argv)
}
