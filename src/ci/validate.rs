//! `just` fallback closure validation (F-012): reject a fallback
//! justfile that is not SELF-CONTAINED — `set fallback`/`set dotenv-*`/
//! `set shell`/`set script-interpreter`, any `import`/`mod` source, and
//! `script`-mode recipe attributes must not let mutable external bytes
//! or a PATH-resolved interpreter fake a green Check.

/// True if the byte at `i` is escaped by an ODD number of immediately
/// preceding backslashes (Just's quoting rule): one `\` escapes the quote, two
/// `\\` do NOT (the string is closed), three do, and so on.
fn is_escape_at(bytes: &[u8], i: usize) -> bool {
    let mut j = i;
    let mut count = 0usize;
    while j > 0 && bytes[j - 1] == b'\\' {
        count += 1;
        j -= 1;
    }
    count % 2 == 1
}

/// True if the byte at `i` CLOSES an open quote of type `q`. Just single-quoted
/// strings are RAW: a `\` never escapes the closing `'`, so a single quote ALWAYS
/// closes; double-quoted strings honor backslash parity (a `\` escapes the closing
/// `"`). Single source of truth for the quote rule shared by every scanner here.
fn closes_quote(bytes: &[u8], i: usize, q: u8) -> bool {
    bytes[i] == q && (q == b'\'' || !is_escape_at(bytes, i))
}

fn find_unquoted_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if closes_quote(bytes, i, q) {
                quote = None;
            }
        } else if c == b'\'' || c == b'"' {
            quote = Some(c);
        } else if c == b':' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_refused_set_directive(line: &str) -> bool {
    // A RECIPE HEADER is `name(args):` — the colon TERMINATES the recipe
    // signature and dependencies may follow (`set fallback: dep`). A `set`
    // SETTING's colon is part of `:=`. So: a `:` that is NOT the start of `:=`
    // marks a recipe header, which is not a refused setting.
    if let Some(cp) = find_unquoted_colon(line) {
        if !line[cp..].starts_with(":=") {
            return false;
        }
    }
    if line.trim_end().ends_with(':') {
        return false;
    }
    // `set` may be followed by a SPACE or a TAB (`set\tfallback` is accepted by
    // Just); strip the keyword and skip exactly one whitespace run.
    let rest = match line.strip_prefix("set") {
        Some(r) if r.starts_with(' ') || r.starts_with('\t') => r.trim_start(),
        _ => return false,
    };
    // The setting NAME ends at the first whitespace, `=`, or `:` — Just
    // accepts both `set fallback := false` and the compact `set fallback:=false`.
    let setting = rest.split([' ', '\t', '=', ':']).next().unwrap_or("");
    // The load-a-file settings ALWAYS read mutable external bytes (their value
    // names a dotenv file/command), so they are refused unconditionally — even
    // `set dotenv-path := ":= false"`, whose VALUE merely contains `:= false`.
    if matches!(
        setting,
        "dotenv-path" | "dotenv-filename" | "dotenv-command"
    ) {
        return true;
    }
    // A custom interpreter (`set shell` / `set script-interpreter`) is either an
    // arbitrary absolute path or a PATH-resolved `sh`; `--shell /bin/sh` pins the
    // FORMER shell but not a `[script]` recipe's interpreter, so refuse any custom
    // interpreter rather than run the plan under an untrusted one (F-027).
    if matches!(setting, "shell" | "script-interpreter") {
        return true;
    }
    // The boolean settings are refused UNLESS the RHS is EXACTLY `false`
    // (disables them). A quoted `"false"` is a string, not the boolean, and is
    // refused.
    if matches!(
        setting,
        "fallback"
            | "dotenv"
            | "dotenv-load"
            | "dotenv-required"
            | "dotenv-override"
            | "default-script"
    ) {
        let rhs = rest[setting.len()..]
            .trim()
            .trim_start_matches(['=', ':'])
            .trim();
        let is_false = rhs == "false";
        return !is_false;
    }
    false
}

pub(super) fn validate_justfile_closure(content: &[u8], rel: &str) -> Result<(), String> {
    let text = String::from_utf8_lossy(content);
    // A leading UTF-8 BOM is accepted by Just; `String::trim` does not remove
    // it, so strip it first or a `BOM + import` first line would bypass the
    // directive scan.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text.as_ref());
    // Join backslash-continued LOGICAL lines (Just lexes `\` + newline as
    // whitespace), so `set \` + `dotenv-path := "..."` recombines into a
    // directive the scanner must see. STREAM: validate each completed logical
    // line immediately and retain only the current continuation buffer, so a
    // huge comment-only plan cannot amplify memory before the CI deadline.
    let mut cur = String::new();
    // A recipe-attribute block can span lines (`[` / `script` / `]`); the block
    // content is accumulated here until it closes, then rejected only if it
    // enables script mode (contains `script`).
    let mut attr_buf: Option<String> = None;
    for phys in text.lines() {
        // Just comments end at the PHYSICAL newline; a `\` inside a comment must
        // NOT continue onto the next line. Strip the comment (quote-aware) per
        // physical line BEFORE deciding whether it continues.
        let no_comment = without_comment(phys);
        let trimmed = no_comment.trim_end();
        if trimmed.ends_with('\\') && !trimmed.ends_with("\\\\") {
            cur.push_str(trimmed.trim_end_matches('\\'));
            cur.push(' ');
        } else {
            cur.push_str(phys);
            let logical = std::mem::take(&mut cur);
            check_just_line(&logical, rel, &mut attr_buf)?;
        }
    }
    if !cur.is_empty() {
        check_just_line(&cur, rel, &mut attr_buf)?;
    }
    Ok(())
}

/// Classify one top-level logical line: route a recipe-attribute block (single
/// line or spanning lines) to the script-mode rejection, otherwise delegate to
/// Split `s` on commas that are NOT inside `(...)` group/quote scope — so an
/// attribute list `[confirm("a,b"), script]` is parsed into its real attributes,
/// not split inside the `"a,b"` argument.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0usize;
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if closes_quote(b, i, q) {
                quote = None;
            }
        } else if c == b'\'' || c == b'"' {
            quote = Some(c);
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        } else if c == b',' && depth == 0 {
            out.push(&s[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// True if a recipe-attribute list contains an attribute whose NAME is `script`
/// (the script-mode switch). Parses attribute names rather than substring
/// matching, so a benign `[confirm("transcript")]` or `[group("javascript")]`
/// (attribute name `confirm`/`group`) is NOT misread as script mode. `inner` is
/// the attribute list WITHOUT the outer `[`/`]`.
fn attr_block_has_script(inner: &str) -> bool {
    split_top_level_commas(inner).iter().any(|part| {
        let name = part
            .trim()
            .split(['(', ':', ' '])
            .next()
            .unwrap_or("")
            .trim();
        name == "script"
    })
}

/// [`reject_refused_directive`]. Track a pending attribute block across lines.
fn check_just_line(line: &str, rel: &str, attr_buf: &mut Option<String>) -> Result<(), String> {
    // Use the COMMENT-STRIPPED trimmed text for attribute structure so a
    // trailing comment on a single-line attribute (`[script] # c`) does not
    // make it look like an unclosed multi-line block.
    let t = without_comment(line).trim();
    // A pending multi-line attribute block: keep accumulating until it closes
    // with `]`, then reject only if it enabled script mode.
    if let Some(buf) = attr_buf {
        buf.push(' ');
        buf.push_str(t);
        if t.contains(']') {
            let inner = buf
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'));
            if let Some(inner) = inner {
                if attr_block_has_script(inner) {
                    *attr_buf = None;
                    return Err(format!(
                        "refusing {rel}: a `script` recipe attribute makes the `just check` fallback non-immutable (F-027)"
                    ));
                }
            }
            *attr_buf = None;
        }
        return Ok(());
    }
    // A line (top-level OR `\`-continued into an indented ` [script]`) whose
    // content starts with `[` begins a recipe-attribute block. Reject only
    // blocks that enable script mode (`script`); benign attributes like
    // `[no-exit-message]` do not select a script interpreter and are allowed.
    if t.starts_with('[') {
        if t.ends_with(']') {
            let inner = &t[1..t.len() - 1];
            if attr_block_has_script(inner) {
                return Err(format!(
                    "refusing {rel}: a `script` recipe attribute makes the `just check` fallback non-immutable (F-027)"
                ));
            }
            return Ok(());
        }
        *attr_buf = Some(t.to_string());
        return Ok(());
    }
    reject_refused_directive(line, rel)
}

/// Reject a single top-level (comment-stripped) logical line if it is a refused
/// `set`/`import`/`mod` directive or a `[script]` recipe attribute — the checks
/// that make the `just check` fallback self-contained (F-012/F-027).
fn reject_refused_directive(line: &str, rel: &str) -> Result<(), String> {
    // An INDENTED line is a recipe body (`check:` -> `    import os`), NOT a
    // top-level directive — except a shebang FIRST body line
    // (`#!/usr/bin/env sh`), which makes Just run the recipe as a script via
    // the shebang interpreter (PATH-resolved), bypassing `--shell /bin/sh`.
    // Check the RAW line: `without_comment` strips `#!` as a comment, so only
    // the untouched line retains it.
    let raw_t = line.trim();
    let ls = without_comment(line);
    if ls.chars().next().is_some_and(|c| c == ' ' || c == '\t') {
        if raw_t.starts_with("#!") {
            return Err(format!(
                "refusing {rel}: a shebang recipe runs via a PATH-resolved interpreter (F-027)"
            ));
        }
        return Ok(());
    }
    let t = ls.trim();
    if t.is_empty() {
        return Ok(());
    }
    // A recipe-attribute block (`[script]`, or a list like `[no-exit-message,
    // script]`) switches a recipe to script mode, run by the PATH-resolved
    // `script-interpreter` (default `sh -eu`), which `--shell` does NOT pin.
    // Refuse ANY attribute list that contains `script` so a PATH `sh` shim
    // cannot green a failing script recipe.
    if is_refused_set_directive(t) {
        return Err(format!(
            "refusing {rel}: a `set fallback`/`set dotenv-*`/`set shell`/`set script-interpreter` setting makes the `just check` fallback non-immutable (F-012)"
        ));
    }
    if is_source_directive(t, "import") || is_source_directive(t, "mod") {
        return Err(format!(
            "refusing {rel}: an `import`/`mod` source makes the `just check` fallback non-immutable (F-012)"
        ));
    }
    Ok(())
}

/// True when a comment-stripped `just` line is an `import`/`mod` source
/// directive (not a longer identifier like `important :=` and not a variable
/// assignment like `mod := 'x'`). Such a directive makes the fallback read a
/// second just source whose bytes `just` resolves at execution — which the
/// runner cannot pin, so it is refused (F-012).
fn is_source_directive(line: &str, kw: &str) -> bool {
    // A Just RECIPE HEADER is `name(args):` — the colon TERMINATES the recipe
    // signature and dependencies may follow (`import value: dep`). A `:` that
    // is NOT the start of `:=` therefore marks a recipe header, which is not a
    // source directive. A directive (`import 'p'`) has no such colon.
    // The header colon must be UNQUOTED: a `:` inside a quoted import path
    // (`import '/tmp/gf:external.just'`) is a legal filename character, not a
    // recipe header — so only an unquoted colon is considered.
    if let Some(cp) = find_unquoted_colon(line) {
        if !line[cp..].starts_with(":=") {
            return false;
        }
    }
    if line.trim_end().ends_with(':') {
        return false;
    }
    let Some(rest0) = line.strip_prefix(kw) else {
        return false;
    };
    // A directive keyword is followed by whitespace or `?` (`import 'x'`,
    // `import? 'x'`, `mod name 'x'`). A recipe named `import:` / `mod-check:`,
    // or a variable `import-cache :=`, is NOT a source directive — the
    // following char is `:`, `-`, `=`, etc.
    match rest0.chars().next() {
        None => return true, // bare `import`/`mod` (just would error -> non-green)
        Some(c) if c == ' ' || c == '\t' || c == '?' => {}
        _ => return false,
    }
    let mut rest = rest0.trim_start();
    if let Some(after_q) = rest.strip_prefix('?') {
        rest = after_q.trim_start();
    }
    // A variable assignment (`import = 'x'` / `mod := 'x'`) is not a directive.
    if rest.starts_with('=') || rest.starts_with(":=") {
        return false;
    }
    true
}

/// Strip a `#` comment from a Just source line WITHOUT removing a `#` that sits
/// inside a single- or double-quoted string (so `import '/tmp/#x.just'` keeps
/// its path and is still recognized as an `import` directive).
fn without_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if closes_quote(bytes, i, q) {
                quote = None;
            }
        } else if c == b'\'' || c == b'"' {
            quote = Some(c);
        } else if c == b'#' {
            return &line[..i];
        }
        i += 1;
    }
    line
}
