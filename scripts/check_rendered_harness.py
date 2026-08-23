#!/usr/bin/env python3
"""Deterministically check eng-init rendered repository artifacts.

This is an eval helper, not part of generated target repos. It validates the
properties that should never depend on LLM judgment: no unresolved eng-init
placeholders, Verification Matrix commands resolve through the selected
justfile/Makefile/package-script entry point, refactor contract commands are
backed by the matrix, repair-mode
ownership is registry-backed, and rehabilitation state is machine-readable.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gate_output import report  # noqa: E402

ANY_PLACEHOLDER = re.compile(r"\{\{[^{}\r\n]+\}\}")
UPPER_PLACEHOLDER = re.compile(r"\{\{[A-Z0-9_]+\}\}")
SECTION_RE_TEMPLATE = r"(?ms)^## {title}\s*$\n(?P<body>.*?)(?=^##\s+|\Z)"
HEADING_RE = re.compile(r"(?m)^##\s+(.+?)\s*$")
JUST_RECIPE_RE = re.compile(r"(?m)^([A-Za-z0-9_.%/-]+)(?:\s+[^:\n]*)?:(?!=)")
MAKE_RECIPE_RE = re.compile(r"(?m)^([A-Za-z0-9_.%/-][A-Za-z0-9_.%/-]*(?:\s+[A-Za-z0-9_.%/-]+)*)\s*:(?!=)")
BACKTICK_COMMAND = re.compile(r"`([^`]+)`")
INLINE_COMMAND = re.compile(r"\b((?:just|make)\s+[A-Za-z0-9_.%/-]+|(?:npm|pnpm|yarn)(?:\s+run)?\s+[A-Za-z0-9_.:%/-]+)\b")
GENERATED_TITLE_RE = re.compile(r"(?m)^\s*-\s*title:\s*[\"']([^\"']+)[\"']")
ROOT_BACKUP_RE = re.compile(r"(^|.*)(\.bak(\.|$)|_backup\b|_old\b|_copy\b)")
SUPPORTED_COMMANDS = {"just", "make", "npm", "pnpm", "yarn"}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def read_text(path: Path, errors: list[str], *, required: bool = True) -> str:
    if not path.exists():
        if required:
            fail(errors, f"missing required file: {path}")
        return ""
    return path.read_text(encoding="utf-8", errors="replace")


def section(markdown: str, title: str) -> str:
    pattern = SECTION_RE_TEMPLATE.format(title=re.escape(title))
    match = re.search(pattern, markdown)
    return match.group("body") if match else ""


def top_level_yaml_block(text: str, key: str) -> str:
    match = re.search(rf"(?ms)^{re.escape(key)}:\s*\n(?P<body>.*?)(?=^[A-Za-z0-9_-]+:.*$|\Z)", text)
    return match.group("body") if match else ""


def nested_yaml_block(parent_block: str, key: str, indent: int = 2) -> str:
    lines = parent_block.splitlines()
    prefix = " " * indent
    child_prefix = " " * (indent + 2)
    for i, line in enumerate(lines):
        match = re.match(rf"^{re.escape(prefix)}{re.escape(key)}:\s*(.*)$", line)
        if not match:
            continue
        inline = match.group(1).strip()
        if inline:
            return inline
        body: list[str] = []
        for next_line in lines[i + 1 :]:
            if next_line.startswith(child_prefix) or not next_line.strip():
                body.append(next_line)
                continue
            break
        return "\n".join(body)
    return ""


# Path tokens are intentionally conservative: root config files and common
# config file extensions count as files, but dotted YAML keys such as
# `size_limits.max_pr_diff_lines` do not.
PROSE_SLASH_SHORTHAND = re.compile(r"^[A-Za-z]+/[A-Za-z]+$")


def is_path_token(token: str) -> bool:
    if command_kind_and_target(token) is not None:
        return False
    if any(ch.isspace() for ch in token):
        return False
    if token.startswith(("http://", "https://", "repos/", "api/")):
        return False
    if token.startswith("."):
        return True
    if "/" in token:
        # `A/B` with two purely alphabetic segments is prose shorthand
        # ("parsed by CI/lint configs", "and/or"), not a path claim.
        # Known blind spot: an extensionless real directory written as
        # `docs/runbooks` reads as prose too — write it as `docs/runbooks/`
        # (trailing slash) so it stays a checkable path claim.
        return not PROSE_SLASH_SHORTHAND.match(token)
    known_root_files = {
        "AGENTS.md",
        "CONTEXT.md",
        "constraints.yaml",
        "justfile",
        "Makefile",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "settings.gradle",
        "tsconfig.json",
    }
    known_extensions = {".js", ".cjs", ".mjs", ".ts", ".tsx", ".json", ".toml", ".yaml", ".yml", ".xml", ".gradle", ".mod", ".ini", ".cfg", ".conf"}
    return token in known_root_files or any(token.endswith(ext) for ext in known_extensions)


def check_path_token(repo: Path, token: str, context: str, errors: list[str]) -> None:
    if is_path_token(token) and not (repo / token).exists():
        fail(errors, f"Enforcement Index path `{token}` does not exist for {context}")

def split_make_targets(target_field: str) -> set[str]:
    return {target for target in target_field.split() if target and not target.startswith(".")}



def command_targets(repo: Path) -> dict[str, set[str]]:
    targets: dict[str, set[str]] = {"just": set(), "make": set(), "package": set()}
    justfile = repo / "justfile"
    if justfile.exists():
        targets["just"] = {m.group(1) for m in JUST_RECIPE_RE.finditer(justfile.read_text(encoding="utf-8", errors="replace"))}
    makefile = repo / "Makefile"
    if makefile.exists():
        targets["make"] = {target for m in MAKE_RECIPE_RE.finditer(makefile.read_text(encoding="utf-8", errors="replace")) for target in split_make_targets(m.group(1))}
    package_json = repo / "package.json"
    if package_json.exists():
        try:
            data = json.loads(package_json.read_text(encoding="utf-8"))
            scripts = data.get("scripts", {})
            if isinstance(scripts, dict):
                targets["package"] = set(scripts.keys())
        except json.JSONDecodeError:
            targets["package"] = set()
    return targets


def looks_like_supported_command(text: str) -> bool:
    parts = text.split()
    return len(parts) >= 2 and parts[0] in SUPPORTED_COMMANDS


def extract_commands(text: str) -> list[str]:
    commands: list[str] = []
    seen: set[str] = set()
    for cmd in BACKTICK_COMMAND.findall(text):
        stripped = cmd.strip()
        if looks_like_supported_command(stripped) and stripped not in seen:
            commands.append(stripped)
            seen.add(stripped)
    for match in INLINE_COMMAND.finditer(text):
        command = match.group(1)
        if command not in seen:
            commands.append(command)
            seen.add(command)
    return commands


def source_table_commands(source: str) -> list[str]:
    commands: list[str] = []
    seen: set[str] = set()
    for line in source.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|") or "---" in stripped:
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if len(cells) < 3 or cells[0].lower() == "surface":
            continue
        for command in extract_commands(cells[2]):
            if command not in seen:
                commands.append(command)
                seen.add(command)
    return commands

def table_column_commands(markdown: str, column_index: int) -> list[str]:
    commands: list[str] = []
    seen: set[str] = set()
    for line in markdown.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|") or "---" in stripped:
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if len(cells) <= column_index or cells[0].lower() in {"surface", "action"}:
            continue
        for command in extract_commands(cells[column_index]):
            if command not in seen:
                commands.append(command)
                seen.add(command)
    return commands


def verification_matrix_commands(matrix: str) -> list[str]:
    commands: list[str] = []
    seen: set[str] = set()
    active_command_column: int | None = None
    for line in matrix.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|"):
            active_command_column = None
            continue
        if "---" in stripped:
            continue
        cells = [cell.strip().lower() for cell in stripped.strip("|").split("|")]
        if "command" in cells:
            active_command_column = cells.index("command")
            continue
        if active_command_column is None or len(cells) <= active_command_column:
            continue
        raw_cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        for command in extract_commands(raw_cells[active_command_column]):
            if command not in seen:
                commands.append(command)
                seen.add(command)
    return commands


def constraints_verification_commands(repo: Path, errors: list[str]) -> list[str]:
    constraints = repo / "constraints.yaml"
    if not constraints.exists():
        return []
    text = constraints.read_text(encoding="utf-8", errors="replace")
    block = top_level_yaml_block(text, "verification")
    if not block:
        return []
    commands: list[str] = []
    seen: set[str] = set()
    # Accept any YAML quoting style: a repo-native formatter (prettier with
    # singleQuote, yamlfmt) may re-quote the file, and quoting is not semantics.
    # The backreference keeps the closing quote matched to the opening one.
    for match in re.finditer(
        r"""(?m)^\s+command:\s+(?P<q>["']?)(?P<cmd>[^"'\n#]+)(?P=q)\s*(?:#.*)?$""", block
    ):
        command = match.group("cmd").strip()
        if not command or command.lower() in {"none", "null"}:
            continue
        if command_kind_and_target(command) is None:
            fail(errors, f"constraints.yaml verification command `{command}` is not a supported command")
            continue
        if command not in seen:
            commands.append(command)
            seen.add(command)
    return commands


def path_tokens(text: str) -> set[str]:
    tokens = set(BACKTICK_COMMAND.findall(text))
    tokens.update(re.findall(r"[A-Za-z0-9_./%-]+", text))
    return tokens




def command_kind_and_target(command: str) -> tuple[str, str] | None:
    parts = command.split()
    if len(parts) < 2 or parts[0] not in SUPPORTED_COMMANDS:
        return None
    kind = parts[0]
    if kind in {"just", "make"}:
        return kind, parts[1]
    if kind in {"npm", "pnpm"}:
        if len(parts) >= 3 and parts[1] == "run":
            return "package", parts[2]
        if kind == "npm" and parts[1] not in {"test", "start", "stop", "restart"}:
            return None
        if kind == "pnpm" and parts[1] in {"add", "approve-builds", "audit", "ci", "config", "create", "dlx", "env", "exec", "install", "link", "list", "outdated", "pack", "patch", "publish", "rebuild", "remove", "setup", "store", "update", "why"}:
            return None
        return "package", parts[1]
    if kind == "yarn":
        if len(parts) >= 3 and parts[1] == "run":
            return "package", parts[2]
        if parts[1] in {"add", "bin", "cache", "config", "dlx", "exec", "explain", "install", "link", "node", "npm", "pack", "patch", "plugin", "remove", "set", "up", "why", "workspace", "workspaces"}:
            return None
        return "package", parts[1]
    return None


def command_exists(targets: dict[str, set[str]], command: str) -> bool:
    parsed = command_kind_and_target(command)
    if parsed is None:
        return False
    kind, target = parsed
    return target in targets.get(kind, set())


COMMAND_ENTRYPOINT_RE = re.compile(r"(?m)^\s*command_entrypoint:\s*[\"']?([^\"'#\n]+)")


def selected_command_kind(repo: Path, errors: list[str]) -> str | None:
    """Return the command surface selected by constraints.yaml, when declared."""
    constraints = repo / "constraints.yaml"
    if not constraints.exists():
        return None
    text = constraints.read_text(encoding="utf-8", errors="replace")
    targets = command_targets(repo)
    kinds: list[str] = []
    for match in COMMAND_ENTRYPOINT_RE.finditer(text):
        value = match.group(1).strip()
        if not value or value.lower() in {"none", "missing"}:
            continue
        parsed = command_kind_and_target(value)
        if parsed is None:
            fail(errors, f"selected command_entrypoint `{value}` is not a supported command")
            continue
        if not command_exists(targets, value):
            fail(errors, f"selected command_entrypoint `{value}` has no matching recipe target")
        kinds.append(parsed[0])
    unique = set(kinds)
    if len(unique) > 1:
        fail(errors, f"conflicting selected command entry points in constraints.yaml: {', '.join(sorted(unique))}")
        return None
    return kinds[0] if kinds else None


def check_selected_command(command: str, selected_kind: str | None, errors: list[str], context: str) -> None:
    parsed = command_kind_and_target(command)
    if parsed is None or selected_kind is None:
        return
    kind, _ = parsed
    if kind != selected_kind:
        fail(errors, f"{context} command `{command}` does not use selected `{selected_kind}` entry point")


def command_target_name(command: str) -> str | None:
    parsed = command_kind_and_target(command)
    return parsed[1] if parsed else None


# Placeholder scanning is scoped to files eng-init writes. A target repository
# legitimately contains `{{...}}` in its own product code (Go composite
# literals `[]T{{...}}`, Go/Jinja/Handlebars templates); scanning everything
# buries the real failures under hundreds of false ones. `--scan-all` restores
# whole-repo scanning for the rare case where that is genuinely wanted.
ENG_INIT_OWNED_NAMES = {
    "AGENTS.md",
    "CONTEXT.md",
    "constraints.yaml",
    "justfile",
    "Makefile",
    ".editorconfig",
    ".gitmessage",
    ".gitignore",
    ".tool-versions",
    "test-guardrails.sh",
    "check-naming.sh",
    "smoke.sh",
    "change-scope.sh",
    "selfcheck.sh",
}
ENG_INIT_OWNED_DIRS = {".github", ".git-hooks", ".husky", ".claude", "agent_tasks", "smoke", "e2e"}


def is_eng_init_artifact(path: Path, repo: Path) -> bool:
    rel = path.relative_to(repo)
    if rel.name in ENG_INIT_OWNED_NAMES:
        return True
    if rel.parts and rel.parts[0] in ENG_INIT_OWNED_DIRS:
        return True
    if len(rel.parts) >= 2 and rel.parts[0] == "docs" and rel.parts[1] == "decisions":
        return True
    return rel.suffix in {".hurl", ".http"}


def is_binary(path: Path) -> bool:
    try:
        return b"\x00" in path.open("rb").read(4096)
    except OSError:
        return True


def check_no_unresolved(repo: Path, errors: list[str], *, scan_all: bool = False) -> None:
    for path in repo.rglob("*"):
        if not path.is_file():
            continue
        if any(part in {".git", "node_modules", ".venv", "target", "dist", "build"} for part in path.parts):
            continue
        if not scan_all and not is_eng_init_artifact(path, repo):
            continue
        if is_binary(path):
            continue
        text = path.read_text(encoding="utf-8", errors="ignore")
        for match in ANY_PLACEHOLDER.finditer(text):
            if match.start() > 0 and text[match.start() - 1] == "$" and is_github_workflow(path):
                continue
            placeholder = match.group(0)
            if UPPER_PLACEHOLDER.fullmatch(placeholder):
                fail(errors, f"unresolved eng-init placeholder {placeholder} in {path.relative_to(repo)}")
            elif not allows_runtime_placeholders(path):
                fail(errors, f"runtime placeholder {placeholder} is only allowed in tool consumer files, found in {path.relative_to(repo)}")


def is_github_workflow(path: Path) -> bool:
    parts = path.parts
    return len(parts) >= 3 and ".github" in parts and "workflows" in parts and path.suffix in {".yml", ".yaml"}




def allows_runtime_placeholders(path: Path) -> bool:
    return path.name in {"justfile", "Makefile"} or path.suffix in {".hurl", ".http"}




def workflow_jobs(text: str) -> dict[str, str]:
    """Map job name -> job body. Hand-rolled: this script stays dependency-free."""
    jobs_block = top_level_yaml_block(text, "jobs")
    if not jobs_block:
        return {}
    jobs: dict[str, str] = {}
    name = None
    body: list[str] = []
    for line in jobs_block.splitlines():
        match = re.match(r"^  ([A-Za-z0-9_.-]+):\s*$", line)
        if match:
            if name is not None:
                jobs[name] = "\n".join(body)
            name = match.group(1)
            body = []
            continue
        if name is not None:
            body.append(line)
    if name is not None:
        jobs[name] = "\n".join(body)
    return jobs


def gates_pull_requests(text: str) -> bool:
    """True when the workflow runs on every pull request.

    Manual (`workflow_dispatch`), scheduled, and tag/release workflows are not
    PR gates, so branch protection never points at them and they need no
    aggregator.
    """
    head = text.split("jobs:", 1)[0]
    return re.search(r"(?m)^\s*(pull_request|pull_request_target)\s*:?\s*$", head) is not None or re.search(
        r"(?m)^on:.*\bpull_request\b", head
    ) is not None


def has_conforming_aggregator(text: str) -> bool:
    jobs = workflow_jobs(text)
    for body in jobs.values():
        if "needs:" not in body:
            continue
        if not re.search(r"^\s*if:\s*always\(\)", body, re.MULTILINE):
            continue
        # The word "skipped" in a step *name* proves nothing; the result
        # matcher itself must include it.
        if re.search(r"^.*result.*skipped.*$", body, re.MULTILINE | re.IGNORECASE):
            return True
    return False


def check_ci_aggregator(repo: Path, errors: list[str]) -> None:
    """Require the always()-guarded aggregator that branch protection points at.

    GitHub counts a skipped required check as passing, so per-job required
    checks are silently disabled by a dependency failure. The aggregator is the
    only shape that closes that hole — and it only closes it when it runs
    unconditionally and treats `skipped` as failure.

    This is repository-scope, matching the `ci_aggregator_gate` criterion:
    branch protection points at one check, so one conforming aggregator anywhere
    satisfies the repo. Auxiliary workflows (manual builds, releases, scheduled
    jobs) are not PR gates and are never required to carry one.
    """
    workflow_dir = repo / ".github" / "workflows"
    workflows = sorted(workflow_dir.glob("*.y*ml")) if workflow_dir.is_dir() else []
    if not workflows:
        return

    pr_gates: list[Path] = []
    for workflow in workflows:
        text = workflow.read_text(encoding="utf-8", errors="replace")
        if has_conforming_aggregator(text):
            return
        if gates_pull_requests(text) and len(workflow_jobs(text)) >= 2:
            pr_gates.append(workflow)

    if not pr_gates:
        return

    near_miss = None
    for workflow in pr_gates:
        jobs = workflow_jobs(workflow.read_text(encoding="utf-8", errors="replace"))
        named = [n for n in jobs if "all-checks" in n or "all_checks" in n]
        if named:
            near_miss = (workflow, named[0], jobs[named[0]])
            break

    if near_miss:
        workflow, name, body = near_miss
        rel = workflow.relative_to(repo)
        if not re.search(r"^\s*if:\s*always\(\)", body, re.MULTILINE):
            fail(errors, f"{rel}: aggregator job `{name}` is missing `if: always()` — a dependency failure would skip it, and GitHub counts a skipped required check as passing")
        else:
            fail(errors, f"{rel}: aggregator `{name}` does not treat `skipped` as failure — that is the hole it exists to close")
        return

    names = ", ".join(str(w.relative_to(repo)) for w in pr_gates)
    fail(errors, f"no CI aggregator job in any PR-gating workflow ({names}) — add an `all-checks-passed` job with `if: always()` whose needs covers every blocking job, or branch protection cannot be made honest")


def check_root_backups(repo: Path, errors: list[str]) -> None:
    for path in repo.iterdir():
        if path.name == ".eng-init":
            continue
        if ROOT_BACKUP_RE.search(path.name):
            fail(errors, f"root-level backup/copy artifact is forbidden by default: {path.name}")


def check_generated_section_registry(repo: Path, agents: str, errors: list[str]) -> None:
    constraints = read_text(repo / "constraints.yaml", errors)
    if not constraints:
        return
    generated_block = top_level_yaml_block(constraints, "generated_sections")
    if not generated_block:
        fail(errors, "constraints.yaml missing generated_sections registry")
        return
    agents_md_block = nested_yaml_block(generated_block, "agents_md")
    if not agents_md_block or agents_md_block.strip() == "[]":
        fail(errors, "generated_sections.agents_md contains no registered titles")
        return
    if not re.search(r"(?m)^  preserve_unknown_sections:\s+true\s*(?:#.*)?$", generated_block):
        fail(errors, "generated section registry must preserve unknown sections by default at generated_sections level")
    registered = GENERATED_TITLE_RE.findall(agents_md_block)
    if not registered:
        fail(errors, "generated_sections.agents_md contains no registered titles")
        return
    headings = set(HEADING_RE.findall(agents))
    expected_generated = {
        "Code Canonicality",
        "Project Identity",
        "Stack & Versions",
        "Directory Map",
        "Development Workflow",
        "Verification Matrix",
        "Source of Truth & Refactor Contract",
        "Important Development Notes",
        "Conventions",
        "Code Review Self-Check",
        "Architecture Discipline",
        "Critical Paths",
        "Observability",
        "Agent Operating Rules",
        "Runtime Lifecycle",
        "Enforcement Index",
    }
    registered_set = set(registered)
    for title in sorted((headings & expected_generated) - registered_set):
        fail(errors, f"generated AGENTS.md section is not registered in generated_sections.agents_md: {title}")
    for title in registered:
        if title not in headings:
            fail(errors, f"generated section registry title not present in AGENTS.md: {title}")


def check_rehabilitation_state(repo: Path, agents: str, errors: list[str]) -> None:
    constraints = read_text(repo / "constraints.yaml", errors)
    selected_kind = selected_command_kind(repo, errors)
    block = top_level_yaml_block(constraints, "rehabilitation")
    if not block:
        fail(errors, "constraints.yaml missing rehabilitation state")
        return
    entry_match = re.search(r'(?m)^\s+command_entrypoint:\s+"?([^"\n#]*)"?\s*(?:#.*)?$', block)
    entry_command = entry_match.group(1).strip() if entry_match else ""
    if not entry_command or entry_command in {"none", "null"}:
        fail(errors, "rehabilitation command_entrypoint must name a concrete selected-entry command")
    if not re.search(r"(?m)^\s+active:\s+true\s*(?:#.*)?$", block):
        fail(errors, "rehabilitation state must be active for this fixture")
    baseline_frozen = bool(re.search(r"(?m)^\s+baseline_frozen:\s+true\s*(?:#.*)?$", block))
    runtime_match = re.search(r'(?m)^\s+runtime_verifier:\s+"?([^"\n#]+)"?\s*(?:#.*)?$', block)
    runtime_command = runtime_match.group(1).strip() if runtime_match else ""
    runtime_verifier = bool(runtime_command and runtime_command not in {"none", "null"})
    broad_true = bool(re.search(r"(?m)^\s+broad_refactor_allowed:\s+true\s*(?:#.*)?$", block))
    broad_false = bool(re.search(r"(?m)^\s+broad_refactor_allowed:\s+false\s*(?:#.*)?$", block))
    if not broad_true and not broad_false:
        fail(errors, "rehabilitation state missing broad_refactor_allowed boolean")
    if broad_true and not (baseline_frozen and runtime_verifier):
        fail(errors, "rehabilitation cannot allow broad refactor before baseline and runtime verifier exist")
    if runtime_verifier:
        check_selected_command(runtime_command, selected_kind, errors, "rehabilitation runtime_verifier")
        if command_kind_and_target(runtime_command) is None:
            fail(errors, f"rehabilitation runtime_verifier `{runtime_command}` is not a supported command (`just`, `make`, or package script)")
        elif not command_exists(command_targets(repo), runtime_command):
            fail(errors, f"rehabilitation runtime_verifier `{runtime_command}` has no matching recipe target")
    for key in ("phase:", "baseline_frozen:", "command_entrypoint:", "runtime_verifier:", "work_unit_protocol:"):
        if key not in block:
            fail(errors, f"rehabilitation state missing key: {key}")
    if "Rehabilitation gate" not in agents:
        fail(errors, "AGENTS.md missing Rehabilitation gate while rehabilitation state is active")


def check_canonicality_first(agents: str, errors: list[str]) -> None:
    """When Code Canonicality is present it must be the first ## section.

    Section order is fixed regardless of mode: an agent that reads the top of the
    file and stops must already know parallel _v1/_v2 implementations are banned.
    Field defect: a rendered file opened with ## Project Identity and pushed
    Code Canonicality down, which prose review had not caught.

    Absence is deliberately NOT failed here. In repair mode eng-init preserves a
    user-owned AGENTS.md it does not own, and demanding its own section ordering
    on that file would push an agent to reorder user content — the mirror of
    relaxing a target repo's gate. Presence is required separately, via
    `--require-section "Code Canonicality"` — which the Stage 5 command passes
    *only when eng-init owns the whole AGENTS.md* (greenfield, bootstrap, or an
    AGENTS.md-scoped repair). See SKILL.md § Validation contract. Do not restate
    that condition as unconditional: a repair scoped to one signal on a
    user-owned file deliberately omits the flag, so on that path presence is not
    required by anything, and this function's tolerance of absence is the whole
    behaviour rather than a gap some later command closes.
    """
    headings = HEADING_RE.findall(agents)
    if "Code Canonicality" not in headings:
        return
    if headings[0] != "Code Canonicality":
        fail(
            errors,
            "AGENTS.md section order: ## Code Canonicality must be the first section, "
            f"found '{headings[0]}' first (it appears at position {headings.index('Code Canonicality') + 1})",
        )


def check_preserved_sections(agents: str, args: argparse.Namespace, errors: list[str]) -> None:
    headings = set(HEADING_RE.findall(agents))
    for title in args.require_preserved_section:
        if title not in headings:
            fail(errors, f"preserved user-owned section missing from AGENTS.md: {title}")


def check_enforcement_index(repo: Path, agents: str, errors: list[str], selected_kind: str | None) -> None:
    index = section(agents, "Enforcement Index")
    if not index:
        fail(errors, "AGENTS.md missing ## Enforcement Index")
        return

    targets = command_targets(repo)
    rows = [line.strip() for line in index.splitlines() if line.strip().startswith("|") and "---" not in line]
    checked_rows = 0
    for row in rows[1:]:
        cells = [cell.strip() for cell in row.strip("|").split("|")]
        if len(cells) < 4:
            continue
        rule, where, checked_by, level = cells[0], cells[1], cells[2], cells[3].lower()
        if "block" not in level and "gate" not in level:
            continue
        checked_rows += 1
        if not checked_by or checked_by in {"-", "—", "todo", "tbd"}:
            fail(errors, f"Enforcement Index {level} row has empty checker: {rule}")
        if "review-only" in checked_by.lower() or "not wired" in checked_by.lower() or "advisory" in checked_by.lower():
            fail(errors, f"Enforcement Index {level} row is marked as non-blocking: {rule}")
        commands = extract_commands(checked_by)
        if not commands and not re.search(r"\b(CI|pre-commit|commit-msg|server-side|GitHub rulesets)\b", checked_by, re.IGNORECASE):
            fail(errors, f"Enforcement Index {level} row has no runnable checker: {rule}")
        for command in commands:
            check_selected_command(command, selected_kind, errors, "Enforcement Index")
            if not command_exists(targets, command):
                fail(errors, f"Enforcement Index command `{command}` has no matching recipe target")
        for token in path_tokens(where + " " + checked_by):
            check_path_token(repo, token, f"{level} row `{rule}`", errors)

    if checked_rows == 0:
        fail(errors, "Enforcement Index contains no block/gate rows to verify")


def check_agents(repo: Path, args: argparse.Namespace, errors: list[str]) -> None:
    agents = read_text(repo / "AGENTS.md", errors)
    if not agents:
        return

    line_count = len(agents.splitlines())
    if args.max_agents_lines and line_count > args.max_agents_lines:
        fail(errors, f"AGENTS.md has {line_count} lines, exceeds {args.max_agents_lines}")

    for required in args.require_section:
        if f"## {required}" not in agents:
            fail(errors, f"AGENTS.md missing required section: {required}")

    check_canonicality_first(agents, errors)
    check_preserved_sections(agents, args, errors)

    matrix = section(agents, "Verification Matrix")
    if not matrix:
        # Absence is not failed here: an L1 repo legitimately has no matrix, and
        # the checks above (line budget, section order, preserved sections) still
        # apply to it. Presence is demanded by `--require-section "Verification
        # Matrix"`, which the Stage 5 command passes at L2+. Validate what is
        # there; let explicit flags demand what must be there.
        return

    targets = command_targets(repo)
    selected_kind = selected_command_kind(repo, errors)
    if not targets["just"] and not targets["make"] and not targets["package"]:
        fail(errors, "justfile/Makefile/package.json scripts missing or contain no runnable targets, but Verification Matrix exists")
    if selected_kind is not None and not targets[selected_kind]:
        fail(errors, f"selected `{selected_kind}` entry point has no runnable targets")

    matrix_commands = verification_matrix_commands(matrix)
    if not matrix_commands:
        fail(errors, "Verification Matrix contains no supported `just ...`, `make ...`, or package-script commands")

    matrix_command_set = set(matrix_commands)
    constraints_commands = constraints_verification_commands(repo, errors)
    matrix_kinds = {command_kind_and_target(command)[0] for command in matrix_commands if command_kind_and_target(command) is not None}
    if selected_kind is None and len(matrix_kinds) > 1:
        fail(errors, f"Verification Matrix mixes command entry points without a selected command_entrypoint: {', '.join(sorted(matrix_kinds))}")
    effective_kind = selected_kind or (next(iter(matrix_kinds)) if len(matrix_kinds) == 1 else None)
    for command in matrix_commands:
        check_selected_command(command, selected_kind, errors, "Verification Matrix")
        if not command_exists(targets, command):
            fail(errors, f"Verification Matrix command `{command}` has no matching recipe target")
    for command in constraints_commands:
        check_selected_command(command, effective_kind, errors, "constraints.yaml verification")
        if command not in matrix_command_set:
            fail(errors, f"constraints.yaml verification command `{command}` is not present in Verification Matrix")
        if not command_exists(targets, command):
            fail(errors, f"constraints.yaml verification command `{command}` has no matching recipe target")
    if constraints_commands:
        constraints_command_set = set(constraints_commands)
        for command in matrix_commands:
            if command not in constraints_command_set:
                fail(errors, f"Verification Matrix command `{command}` is not mirrored in constraints.yaml verification")

    source = section(agents, "Source of Truth & Refactor Contract")
    if not source:
        if args.require_refactor_contract or args.require_compare:
            fail(errors, "AGENTS.md missing ## Source of Truth & Refactor Contract")
    else:
        source_commands = source_table_commands(source)
        if args.require_compare and not source_commands:
            fail(errors, "refactor contract requires at least one oracle/compare command in the Source of Truth verification table")
        for command in source_commands:
            check_selected_command(command, effective_kind, errors, "Source of Truth")
            if command not in matrix_command_set:
                fail(errors, f"Source of Truth command `{command}` is not present in Verification Matrix")
            if not command_exists(targets, command):
                fail(errors, f"Source of Truth command `{command}` has no matching recipe target")

    if args.require_generated_section_registry:
        check_generated_section_registry(repo, agents, errors)
    if args.require_rehabilitation_state:
        check_rehabilitation_state(repo, agents, errors)
    if args.require_enforcement_index:
        check_enforcement_index(repo, agents, errors, effective_kind)


def main() -> int:
    parser = argparse.ArgumentParser(description="Check rendered eng-init artifacts in a fixture repo")
    parser.add_argument("repo", type=Path)
    parser.add_argument("--max-agents-lines", type=int, default=320)
    parser.add_argument("--require-section", action="append", default=[])
    parser.add_argument("--require-preserved-section", action="append", default=[])
    parser.add_argument("--require-refactor-contract", action="store_true")
    parser.add_argument("--require-compare", action="store_true")
    parser.add_argument("--require-generated-section-registry", action="store_true")
    parser.add_argument("--require-rehabilitation-state", action="store_true")
    parser.add_argument("--require-enforcement-index", action="store_true")
    parser.add_argument("--forbid-root-backups", action="store_true")
    parser.add_argument("--require-ci-aggregator", action="store_true", help="require the always()-guarded CI aggregator job that branch protection points at")
    parser.add_argument("--scan-all", action="store_true", help="scan every file for placeholders, not just eng-init artifacts (noisy on real repos)")
    args = parser.parse_args()

    errors: list[str] = []
    repo = args.repo.resolve()
    if not repo.exists():
        fail(errors, f"repo path does not exist: {repo}")
    else:
        check_no_unresolved(repo, errors, scan_all=args.scan_all)
    # These three are independent of each other. Nesting them under
    # --require-ci-aggregator made every AGENTS.md check silently no-op on any
    # invocation without that flag — including repos with no CI, exactly where
    # the flag is correctly omitted. A checker that passes an unvalidated file
    # is the phantom enforcement this skill exists to prevent.
    if args.require_ci_aggregator:
        check_ci_aggregator(repo, errors)
    if args.forbid_root_backups:
        check_root_backups(repo, errors)
    check_agents(repo, args, errors)

    return report("check-rendered-harness", errors,
                  f"{repo.name}: AGENTS.md, commands, and enforcement wiring conform")


if __name__ == "__main__":
    sys.exit(main())
