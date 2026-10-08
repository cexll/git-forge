//! Strict, dependency-free markdown issue plan input.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Entry {
    pub file: PathBuf,
    pub key: String,
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
    pub open: Option<bool>,
}
impl Entry {
    pub fn error(&self, field: &str, message: &str) -> String {
        format!("{}: {field}: {message}", self.file.display())
    }
}

pub(crate) fn load(path: &Path) -> Result<Vec<Entry>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: plan: {e}", path.display()))?;
    let files = if meta.is_dir() {
        let mut files = Vec::new();
        for item in std::fs::read_dir(path).map_err(|e| format!("{}: plan: {e}", path.display()))? {
            let item = item.map_err(|e| format!("{}: plan: {e}", path.display()))?;
            if item.path().extension().is_some_and(|s| s == "md") {
                if !item.file_type().map_err(|e| e.to_string())?.is_file() {
                    continue;
                }
                files.push(item.path());
            }
        }
        files.sort();
        files
    } else if meta.is_file() {
        vec![path.to_path_buf()]
    } else {
        return Err(format!(
            "{}: plan: expected a file or directory",
            path.display()
        ));
    };
    if files.is_empty() {
        return Err(format!("{}: plan: no markdown entries", path.display()));
    }
    let mut entries = Vec::new();
    let mut keys = HashSet::new();
    for file in files {
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: plan: {e}", file.display()))?;
        let entry = parse(&file, &text)?;
        if !keys.insert(entry.key.clone()) {
            return Err(entry.error("key", "duplicate key within plan"));
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn parse(file: &Path, text: &str) -> Result<Entry, String> {
    let error = |field: &str, message: &str| format!("{}: {field}: {message}", file.display());
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(error("frontmatter", "expected opening ---"));
    }
    let mut offset = text.find('\n').map_or(text.len(), |n| n + 1);
    let mut fields = HashMap::new();
    let mut closed = false;
    for line in lines {
        offset += line.len();
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .ok_or_else(|| error("frontmatter", "expected field: value"))?;
        let field = field.trim();
        let value = strip_comment(value).map_err(|e| error(field, &e))?;
        if !matches!(
            field,
            "key" | "title" | "parent" | "blocked_by" | "labels" | "state"
        ) {
            return Err(error(field, "unknown field"));
        }
        if fields
            .insert(field.to_string(), value.trim().to_string())
            .is_some()
        {
            return Err(error(field, "duplicate field"));
        }
    }
    if !closed {
        return Err(error("frontmatter", "missing closing ---"));
    }
    let scalar_field = |field: &str| -> Result<String, String> {
        scalar(
            fields
                .get(field)
                .ok_or_else(|| error(field, "required field"))?,
        )
        .map_err(|e| error(field, &e))
    };
    let key = scalar_field("key")?;
    if key.starts_with('#') || key.chars().any(|c| c.is_whitespace() || "[],".contains(c)) {
        return Err(error(
            "key",
            "expected a non-empty stable key without whitespace, # prefix, commas or brackets",
        ));
    }
    let title = scalar_field("title")?;
    let list_field = |field: &str| -> Result<Vec<String>, String> {
        fields
            .get(field)
            .map(|v| list(v).map_err(|e| error(field, &e)))
            .unwrap_or(Ok(Vec::new()))
    };
    let parent = fields
        .get("parent")
        .map(|v| scalar(v).map_err(|e| error("parent", &e)))
        .transpose()?;
    let open = fields
        .get("state")
        .map(|v| match scalar(v)?.as_str() {
            "open" => Ok(true),
            "closed" => Ok(false),
            _ => Err("expected open or closed".into()),
        })
        .transpose()
        .map_err(|e: String| error("state", &e))?;
    Ok(Entry {
        file: file.to_path_buf(),
        key,
        title,
        description: text[offset..].to_string(),
        labels: list_field("labels")?,
        parent,
        blocked_by: list_field("blocked_by")?,
        open,
    })
}

fn strip_comment(input: &str) -> Result<String, String> {
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote == Some('"') && c == '\\' {
            escaped = true;
            continue;
        }
        if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote.is_none()
            && c == '#'
            && (i == 0 || input[..i].ends_with(char::is_whitespace))
        {
            return Ok(input[..i].to_string());
        }
    }
    if quote.is_some() {
        return Err("unterminated quoted scalar".into());
    }
    Ok(input.to_string())
}
fn scalar(input: &str) -> Result<String, String> {
    let input = input.trim();
    let value = if input.starts_with('"') {
        match crate::event::parse_json_value(input) {
            Some(crate::event::JsonValue::String(s)) => s,
            _ => return Err("invalid double-quoted scalar".into()),
        }
    } else if input.starts_with('\'') {
        input
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .filter(|s| !s.contains('\''))
            .ok_or_else(|| "invalid single-quoted scalar".to_string())?
            .to_string()
    } else {
        if input.starts_with('#') || input.contains(['[', ']', ',', '\'', '"']) {
            return Err("invalid unquoted scalar".into());
        }
        input.to_string()
    };
    if value.trim().is_empty() {
        return Err("must be non-empty".into());
    }
    Ok(value)
}
fn list(input: &str) -> Result<Vec<String>, String> {
    let inner = input
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| "expected an inline [item, ...] array".to_string())?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in inner.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote == Some('"') && c == '\\' {
            escaped = true;
            continue;
        }
        if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote.is_none() && c == ',' {
            result.push(scalar(&inner[start..i])?);
            start = i + 1;
        }
    }
    result.push(scalar(&inner[start..])?);
    if result.iter().collect::<HashSet<_>>().len() != result.len() {
        return Err("duplicate array item".into());
    }
    Ok(result)
}
