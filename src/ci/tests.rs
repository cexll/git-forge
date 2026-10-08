//! CI plan unit tests (split out of `ci.rs` so it stays within the
//! per-file size gate).
use super::validate::validate_justfile_closure;

// F-012 (review): the optional-import form `import?`, and the aliased
// module form `mod name 'path'`, must be recognized so an absolute/tilde/
// parent-traversing path cannot load mutable bytes outside the snapshot.
#[test]
fn refuses_optional_import_absolute_escape() {
    assert!(validate_justfile_closure(b"import? '/tmp/green.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"mod? name '/tmp/green.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"mod name '/tmp/green.just'", "justfile").is_err());
}

#[test]
fn refuses_any_import_source_directive() {
    // The `just check` fallback must be SELF-CONTAINED: ANY import/mod
    // source directive (explicit, optional, implicit, or parent-traversing)
    // makes the closure non-immutable and is refused (F-012).
    assert!(validate_justfile_closure(b"import '../paren.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"import './../paren.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"import 'checks..common.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"import 'sub/checks.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"import 'x.just'", "justfile").is_err());
    assert!(validate_justfile_closure(b"mod 'sub/checks.just'", "justfile").is_err());
    assert!(
        validate_justfile_closure(b"\xef\xbb\xbfimport '/tmp/green.just'", "justfile").is_err(),
        "a leading UTF-8 BOM must not bypass the directive scan"
    );
    // A recipe named `modx` is not a source directive.
    assert!(validate_justfile_closure(b"modx", "justfile").is_ok());
}

// Review (round 2): a `#` inside a quoted path is NOT a comment — the path
// directive is still recognized (so the fallback is refused); quote
// awareness keeps a `#`-in-path from silently bypassing the check.
#[test]
fn refuses_path_with_quoted_hash() {
    assert!(
        validate_justfile_closure(b"import '/tmp/#green.just'", "justfile").is_err(),
        "a `#` inside quotes must not truncate the path before the directive check"
    );
    assert!(validate_justfile_closure(b"import 'sub/checks.just' # trailing", "justfile").is_err());
}

// Review (round 2): keyword boundary — a variable assignment whose name
// merely contains the `import`/`mod` prefix is NOT a source directive, and
// a self-contained justfile must be accepted.
#[test]
fn allows_variable_assignments_and_self_contained() {
    assert!(
        validate_justfile_closure(b"important := '/tmp/cache'", "justfile").is_ok(),
        "an `important :=` assignment is not an `import` directive"
    );
    assert!(
        validate_justfile_closure(b"module_root := '/tmp/cache'", "justfile").is_ok(),
        "a `module_root :=` assignment is not a `mod` directive"
    );
    assert!(
        validate_justfile_closure(b"check:\n  @sh -c 'exit 1'", "justfile").is_ok(),
        "a self-contained justfile (no import/mod) is accepted"
    );
    assert!(
        validate_justfile_closure(b"check:\n    import os\n    print('x')\n", "justfile").is_ok(),
        "an indented recipe body containing `import os` is NOT a top-level directive"
    );
    assert!(
        validate_justfile_closure(b"import:\n  @sh -c 'exit 1'", "justfile").is_ok(),
        "a recipe named `import:` is not a source directive"
    );
    assert!(
        validate_justfile_closure(b"set dotenv := false", "justfile").is_ok(),
        "`set dotenv := false` disables dotenv and is allowed"
    );
    assert!(
        validate_justfile_closure(b"set fallback:=false", "justfile").is_ok(),
        "compact `set fallback:=false` disables fallback and is allowed"
    );
    assert!(
        validate_justfile_closure(b"set fallback:=true", "justfile").is_err(),
        "compact `set fallback:=true` enables fallback and is refused"
    );
    assert!(
        validate_justfile_closure(b"set dotenv-load:=false", "justfile").is_ok(),
        "compact `set dotenv-load:=false` disables dotenv-load and is allowed"
    );
    assert!(
        validate_justfile_closure(b"set dotenv-path := \":= false\"", "justfile").is_err(),
        "a `set dotenv-path` value naming a file is refused even if it looks like false"
    );
    assert!(
        validate_justfile_closure(b"set\tdotenv-path := '/tmp/x'", "justfile").is_err(),
        "a tab after `set` is accepted by Just and must still be refused"
    );
    assert!(
        validate_justfile_closure(b"set \\\ndotenv-path := '/tmp/x'", "justfile").is_err(),
        "a backslash-newline continuation recombines into a refused `set dotenv-path`"
    );
}

// Review (round 6): a `:` inside a quoted import/mod path is a legal
// filename character, NOT a recipe header — the directive must still be
// recognized and refused so the fallback cannot read mutable external bytes.
#[test]
fn refuses_colon_in_import_path() {
    assert!(
        validate_justfile_closure(b"import '/tmp/gf:external.just'", "justfile").is_err(),
        "an absolute import path containing a colon must be refused"
    );
    assert!(
        validate_justfile_closure(b"mod m '/tmp/gf:external.just'", "justfile").is_err(),
        "a mod path containing a colon must be refused"
    );
    assert!(
        validate_justfile_closure(b"set dotenv-path := '/tmp/a:b'", "justfile").is_err(),
        "a set dotenv-path value containing a colon must be refused"
    );
    // A recipe header's own colon is still a header, not a directive.
    assert!(
        validate_justfile_closure(b"import value: dep", "justfile").is_ok(),
        "a recipe named `import value:` is not a source directive"
    );
}
// Review (round 7): a closing quote preceded by an EVEN number of
// backslashes (`"a\\"`) is a CLOSED string, not an escaped quote — so the
// `#` after it is a real comment, and a following top-level `import` must
// NOT be swallowed by a backslash-continuation. Quote handling uses
// backslash PARITY, not a single-preceding-byte test.
#[test]
fn refuses_import_after_even_backslash_string_comment() {
    assert!(
        validate_justfile_closure(
            b"x := \"a\\\\\" # \\\nimport '/tmp/green.just'\n",
            "justfile"
        )
        .is_err(),
        "a real import after a closed string + comment must still be refused"
    );
}
// Review (round 9): Just single-quoted strings are RAW — a closing quote is
// NOT escaped by a preceding backslash, so a comment after it is recognized
// and a following top-level `import` is not swallowed by a continuation.
#[test]
fn refuses_import_after_raw_single_quote_string_comment() {
    assert!(
        validate_justfile_closure(
            "x := 'foo\\' # \\\nimport '/tmp/green.just'\n".as_bytes(),
            "justfile"
        )
        .is_err(),
        "a real import after a raw single-quoted string + comment must be refused"
    );
}

// Review (round 9): a `[script]` recipe runs the body via the PATH-resolved
// `script-interpreter` (default `sh -eu`), which `--shell` does NOT pin — a
// PATH `sh` shim could green it, so it is refused.
#[test]
fn refuses_script_attribute() {
    assert!(
        validate_justfile_closure(b"[script]\ncheck:\n  exit 1", "justfile").is_err(),
        "a [script] recipe attribute must be refused"
    );
}

// Review (round 9): a custom `set shell` / `set script-interpreter` is an
// untrusted interpreter and is refused rather than silently overridden by
// `--shell /bin/sh` (which would change valid plan semantics).
#[test]
fn refuses_custom_interpreter_setting() {
    assert!(
        validate_justfile_closure(b"set shell := ['/usr/bin/python3', '-c']", "justfile").is_err(),
        "a custom `set shell` must be refused"
    );
    assert!(
        validate_justfile_closure(b"set script-interpreter := 'python3'", "justfile").is_err(),
        "a custom `set script-interpreter` must be refused"
    );
}

// Review (round 10): Just recipe-attribute LISTS (`[no-exit-message, script]`)
// and SHEBANG recipes reach a PATH-resolved script/shebang interpreter, which
// `--shell /bin/sh` does NOT pin — both are refused (fail-closed).
#[test]
fn refuses_script_attribute_list_and_shebang() {
    assert!(
        validate_justfile_closure(b"[no-exit-message, script]\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "a recipe-attribute list containing `script` must be refused"
    );
    assert!(
        validate_justfile_closure(b"check:\n  #!/usr/bin/env sh\n  exit 1", "justfile").is_err(),
        "a shebang recipe must be refused"
    );
}
// Review (round 11): `set default-script := true` (Just 1.52+) makes
// ordinary recipes use the PATH-resolved script-interpreter, ignoring
// `--shell /bin/sh` — refuse an enabled default-script.
// Review (round 11): a `\`-continued attribute (`\` + newline + `[script]`)
// joins into an indented ` [script]`; the RAW-start-[ rejection catches it.
#[test]
fn refuses_enabled_default_script_and_continued_attribute() {
    assert!(
        validate_justfile_closure(b"set default-script := true\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "an enabled `set default-script` must be refused"
    );
    assert!(
        validate_justfile_closure(("\n[script]\ncheck:\n  exit 1").as_bytes(), "justfile").is_err(),
        "a continuation-joined `[script]` attribute must be refused"
    );
    assert!(
        validate_justfile_closure(b"set default-script := false", "justfile").is_ok(),
        "`set default-script := false` disables it and is allowed"
    );
}
// Review (round 12): benign recipe attributes (e.g. `[no-exit-message]`) do
// NOT select a script interpreter and must be ALLOWED, while a
// `[no-exit-message, script]` list and a multiline `[` / `script` / `]`
// block ARE refused.
#[test]
fn allows_benign_attribute_but_refuses_script_block() {
    assert!(
        validate_justfile_closure(b"[no-exit-message]\ncheck:\n  true", "justfile").is_ok(),
        "a benign `[no-exit-message]` attribute must be allowed"
    );
    assert!(
        validate_justfile_closure(b"[no-exit-message, script]\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "a `[no-exit-message, script]` list must be refused"
    );
    assert!(
        validate_justfile_closure(b"[no-exit-message,\n script]\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "a multiline `[ ... script ]` block must be refused"
    );
}

// Review (round 13): a `[script]` attribute with a TRAILING COMMENT
// (`[script] # c`) must not make the block look unclosed (which would
// swallow the following recipe and let script mode bypass the scanner).
#[test]
fn refuses_script_attribute_with_trailing_comment() {
    assert!(
        validate_justfile_closure(b"[script] # trailing\ncheck:\n  exit 1", "justfile").is_err(),
        "a `[script]` attribute with a trailing comment must be refused"
    );
    assert!(
        validate_justfile_closure(
            b"[no-exit-message, script] # trailing\ncheck:\n  exit 1",
            "justfile"
        )
        .is_err(),
        "a `[no-exit-message, script]` list with a trailing comment must be refused"
    );
}
// Review (round 14): attribute-name PARSING, not substring matching — a
// benign attribute whose ARGUMENT contains `script` (`[confirm("transcript")]`,
// `[group("javascript")]`) must be ALLOWED, while a real `script` attribute
// (name) is refused even in a list with a comma in a quoted argument.
#[test]
fn allows_script_in_argument_but_refuses_script_name() {
    assert!(
        validate_justfile_closure(b"[confirm(\"transcript\")]\ncheck:\n  true", "justfile").is_ok(),
        "a benign attribute with `script`-inside-argument must be allowed"
    );
    assert!(
        validate_justfile_closure(b"[group(\"javascript\")]\ncheck:\n  true", "justfile").is_ok(),
        "a benign `group(\"javascript\")` attribute must be allowed"
    );
    assert!(
        validate_justfile_closure(b"[confirm(\"a,b\"), script]\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "a real `script` attribute in a list must be refused even with a comma in an argument"
    );
}
// Review (round 15 / F4): the attribute-list splitter must honor Just's RAW
// single-quote rule (a `\` does NOT escape the closing `'`) — the same rule the
// directive scanner uses. A `script` attribute following a single-quoted
// argument that ENDS in a backslash must still be detected (a splitter that
// treats `\'` as an escape keeps the quote open and swallows the `script`).
#[test]
fn refuses_script_after_backslash_terminated_single_quoted_arg() {
    assert!(
        validate_justfile_closure(b"[confirm('x\\'), script]\ncheck:\n  exit 1", "justfile")
            .is_err(),
        "a `script` attribute after a backslash-terminated single-quoted argument must be refused"
    );
    // Positive control: no `script` attribute -> allowed before and after.
    assert!(
        validate_justfile_closure(
            b"[confirm('x\\'), no-exit-message]\ncheck:\n  true",
            "justfile"
        )
        .is_ok(),
        "a benign attribute list with a backslash-terminated single-quoted arg must be allowed"
    );
}

// ── #38 CI failure reason and bounded output tail ──

use super::output::{bounded_tail, Capture, TAIL_BYTES};
use super::{exit_result, failed_check_error, CiFailure, CiReason};

#[test]
fn bounded_tail_keeps_short_output_exactly() {
    assert_eq!(bounded_tail(b""), "");
    assert_eq!(bounded_tail(b"ok\n"), "ok\n");
    let exact = "a".repeat(4096);
    assert_eq!(bounded_tail(exact.as_bytes()), exact);
}

#[test]
fn bounded_tail_cuts_to_the_last_4096_bytes() {
    let output = format!("X{}", "a".repeat(4096));
    assert_eq!(bounded_tail(output.as_bytes()), "a".repeat(4096));
    let numbered: String = (0..2000).map(|i| format!("{i:04}\n")).collect();
    assert_eq!(
        bounded_tail(numbered.as_bytes()),
        &numbered[numbered.len() - 4096..]
    );
}

#[test]
fn bounded_tail_drops_a_character_split_by_the_cut() {
    // 2-, 3- and 4-byte characters whose first byte falls just before the cut.
    for ch in ["é", "中", "🚀"] {
        let output = format!("{}{}", ch.repeat(3000), "z");
        let tail = bounded_tail(output.as_bytes());
        assert!(tail.len() <= TAIL_BYTES, "{ch}: {}", tail.len());
        assert!(!tail.contains('\u{fffd}'), "{ch}: split sequence decoded");
        assert!(output.ends_with(&tail), "{ch}: tail is a suffix");
        assert!(
            tail.len() > TAIL_BYTES - ch.len(),
            "{ch}: only the split char dropped"
        );
    }
}

#[test]
fn bounded_tail_replaces_invalid_bytes_within_the_bound() {
    assert_eq!(bounded_tail(b"\x80\xff ok\n"), "\u{fffd}\u{fffd} ok\n");
    // Invalid bytes are not continuation bytes of a cut character: 0xFF is
    // never dropped as a partial sequence, it decodes to U+FFFD.
    let flood = vec![0xFFu8; 5000];
    let tail = bounded_tail(&flood);
    assert_eq!(tail, "\u{fffd}".repeat(1365));
    assert!(tail.len() <= TAIL_BYTES);
    // Control bytes stay exact; the terminal escapes them separately.
    assert_eq!(bounded_tail(b"\x1b[31m\x00\n"), "\u{1b}[31m\u{0}\n");
}

/// F-016: the capture keeps at most TAIL_BYTES + 1 bytes in memory however
/// much the plan writes, and its tail equals the tail of the whole output.
#[test]
fn capture_memory_stays_bounded_while_draining() {
    use std::io::Write as _;
    let (capture, mut writer) = Capture::start().unwrap();
    let mut all = Vec::new();
    for i in 0..1024u32 {
        let chunk: Vec<u8> = (0..1024u32).map(|j| ((i * 7 + j) % 251) as u8).collect();
        writer.write_all(&chunk).unwrap();
        all.extend_from_slice(&chunk);
        assert!(
            capture.kept_len() <= TAIL_BYTES + 1,
            "kept {}",
            capture.kept_len()
        );
    }
    drop(writer);
    assert_eq!(capture.finish(), bounded_tail(&all));
}

#[test]
fn exit_status_maps_to_nonzero_exit_or_green() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    assert_eq!(exit_result(ExitStatus::from_raw(0)), Ok(()));
    assert_eq!(
        exit_result(ExitStatus::from_raw(3 << 8)),
        Err(CiFailure::new(
            CiReason::NonzeroExit,
            "exited with status 3"
        ))
    );
    assert_eq!(
        exit_result(ExitStatus::from_raw(9)),
        Err(CiFailure::new(
            CiReason::NonzeroExit,
            "terminated by signal 9"
        ))
    );
}

#[test]
fn reason_names_are_the_wire_values() {
    let names: Vec<&str> = [
        CiReason::PlanInvalid,
        CiReason::SpawnFailed,
        CiReason::Timeout,
        CiReason::NonzeroExit,
        CiReason::CleanupFailed,
    ]
    .into_iter()
    .map(CiReason::as_str)
    .collect();
    assert_eq!(
        names,
        [
            "plan_invalid",
            "spawn_failed",
            "timeout",
            "nonzero_exit",
            "cleanup_failed"
        ]
    );
}

#[test]
fn failed_check_error_escapes_terminal_text_and_keeps_data_exact() {
    let error = failed_check_error(
        4,
        "CI run for PR #4 failed (.forge/ci.sh): exited with status 1".into(),
        CiReason::NonzeroExit,
        "a\u{1b}[2J\nb",
        "hint",
    );
    assert_eq!(error.code, "E_CI_FAILED");
    assert_eq!(
        error.message,
        "CI run for PR #4 failed (.forge/ci.sh): exited with status 1; reason: nonzero_exit; \
         output tail: a\\x1b[2J\\x0ab"
    );
    assert_eq!(
        error.to_json(),
        "{\"error\":{\"code\":\"E_CI_FAILED\",\"message\":\"CI run for PR #4 failed \
         (.forge/ci.sh): exited with status 1; reason: nonzero_exit; output tail: \
         a\\\\x1b[2J\\\\x0ab\",\"input\":\"4\",\"hint\":\"hint\",\"retryable\":false,\
         \"data\":{\"pr\":4,\"reason\":\"nonzero_exit\",\"tail\":\"a\\u001b[2J\\nb\"}}}"
    );
    let empty = failed_check_error(4, "m".into(), CiReason::PlanInvalid, "", "hint");
    assert_eq!(empty.message, "m; reason: plan_invalid");
}
