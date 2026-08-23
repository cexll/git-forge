use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<JsonValue>),
    Object(HashMap<String, JsonValue>),
}

impl JsonValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            JsonValue::Number(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&HashMap<String, JsonValue>> {
        match self {
            JsonValue::Object(m) => Some(m),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    IssueCreated,
    IssueComment,
    IssueClose,
    IssueReopen,
    PrCreated,
    PrComment,
    PrReview,
    PrMerge,
    CiCheck,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::IssueCreated => "issue.created",
            EventKind::IssueComment => "issue.comment",
            EventKind::IssueClose => "issue.close",
            EventKind::IssueReopen => "issue.reopen",
            EventKind::PrCreated => "pr.created",
            EventKind::PrComment => "pr.comment",
            EventKind::PrReview => "pr.review",
            EventKind::PrMerge => "pr.merge",
            EventKind::CiCheck => "ci.check",
        }
    }
}

impl std::str::FromStr for EventKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "issue.created" => Self::IssueCreated,
            "issue.comment" => Self::IssueComment,
            "issue.close" => Self::IssueClose,
            "issue.reopen" => Self::IssueReopen,
            "pr.created" => Self::PrCreated,
            "pr.comment" => Self::PrComment,
            "pr.review" => Self::PrReview,
            "pr.merge" => Self::PrMerge,
            "ci.check" => Self::CiCheck,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub v: u32,
    pub id: String,
    pub kind: EventKind,
    pub entity: String,
    pub entity_id: u64,
    pub ts: String,
    pub actor: String,
    pub body: HashMap<String, JsonValue>,
}

/// Validates RFC 4122 version-4 UUID textual shape: 8-4-4-4-12 hex groups,
/// version nibble 4, variant nibble in [89ab].
pub fn is_uuid_v4(input: &str) -> bool {
    let bytes = input.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match (i, b) {
            (8, _) | (13, _) | (18, _) | (23, _) => {
                if *b != b'-' {
                    return false;
                }
            }
            (_, b) if *b == b'-' => return false,
            (_, b) if b.is_ascii_hexdigit() => {}
            _ => return false,
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn uuid_v4_from_seed(seed: u64) -> String {
    // Deterministic std-only UUID-shaped id. Not cryptographically random:
    // callers that require provenance must pass their own validated UUID.
    let mut state = seed | 1;
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Current system time as an RFC3339 UTC timestamp, seconds granularity
/// (`YYYY-MM-DDTHH:MM:SSZ`). The v1 event schema contract is `"ts":
/// "<RFC3339 UTC>"`; a std-only epoch→civil conversion keeps the schema
/// dependency-free (no chrono).
fn rfc3339_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_rfc3339_utc(secs)
}

/// Format seconds-since-epoch as RFC3339 UTC `YYYY-MM-DDTHH:MM:SSZ`.
fn format_rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Days-since-1970-01-01 → proleptic-Gregorian `(year, month, day)`, the
/// inverse of the standard days-from-civil algorithm (Howard Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Event {
    /// Creates an event with a UUID-v4-shaped id from a std-only seed.
    /// The seed is not a cryptographic entropy source; production id
    /// provenance belongs to the store layer (uuid crate) in later slices.
    pub fn new(
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, JsonValue>,
    ) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ entity_id;
        Self {
            v: 1,
            id: uuid_v4_from_seed(seed),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: rfc3339_utc_now(),
            actor: actor.to_string(),
            body,
        }
    }

    /// Constructs an event with caller-supplied provenance. Rejects non-UUID-v4
    /// ids; the caller owns cryptographic provenance, the schema owner owns shape.
    pub fn new_with_id(
        id: &str,
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, JsonValue>,
    ) -> Option<Self> {
        if !is_uuid_v4(id) {
            return None;
        }
        Some(Self {
            v: 1,
            id: id.to_string(),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: rfc3339_utc_now(),
            actor: actor.to_string(),
            body,
        })
    }

    pub fn from_json(input: &str) -> Option<Self> {
        let root = parse_json_value(input)?;
        let object = root.as_object()?;
        let v = object.get("v")?.as_u64()?;
        if v != 1 {
            return None;
        }
        let id = object.get("id")?.as_str()?.to_string();
        if !is_uuid_v4(&id) {
            return None;
        }
        let kind = object.get("kind")?.as_str()?.parse().ok()?;
        let entity = object.get("entity")?.as_str()?.to_string();
        let entity_id = object.get("entity_id")?.as_u64()?;
        let ts = object.get("ts")?.as_str()?.to_string();
        let actor = object.get("actor")?.as_str()?.to_string();
        let body = match object.get("body")? {
            JsonValue::Object(m) => m.clone(),
            _ => return None,
        };
        Some(Self {
            v: v as u32,
            id,
            kind,
            entity,
            entity_id,
            ts,
            actor,
            body,
        })
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"v\":{},\"id\":{},\"kind\":{},\"entity\":{},\"entity_id\":{},\"ts\":{},\"actor\":{},\"body\":{}}}",
            self.v,
            json_string(&self.id),
            json_string(self.kind.as_str()),
            json_string(&self.entity),
            self.entity_id,
            json_string(&self.ts),
            json_string(&self.actor),
            json_object(&self.body)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub comments: Vec<String>,
    pub open: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub source_ref: Option<String>,
    pub base_ref: Option<String>,
    pub source_head: Option<String>,
    pub base_head: Option<String>,
    pub merge_base: Option<String>,
    pub comments: Vec<String>,
    pub effective_decision: Option<String>,
    pub merge_result: Option<String>,
    /// Latest CI Check status (success/failed/pending) — the fold keeps the
    /// most recently appended `ci.check` event, so the merge gate can read a
    /// single latest status. `None` when no CI Check has been recorded yet.
    pub ci_status: Option<String>,
    /// The plan (`.forge/ci.sh` or `just check`) that produced the latest
    /// CI Check.
    pub ci_plan: Option<String>,
    /// Actor of the latest CI Check.
    pub ci_actor: Option<String>,
    /// Timestamp of the latest CI Check.
    pub ci_ts: Option<String>,
}

impl PrState {
    fn apply(&mut self, event: &Event) {
        let str_field = |m: &HashMap<String, JsonValue>, key: &str| {
            m.get(key).and_then(JsonValue::as_str).map(String::from)
        };
        match event.kind {
            EventKind::PrCreated => {
                self.title = str_field(&event.body, "title");
                self.description = str_field(&event.body, "description");
                self.labels = labels_from(&event.body);
                self.source_ref = str_field(&event.body, "source_ref");
                self.base_ref = str_field(&event.body, "base_ref");
                self.source_head = str_field(&event.body, "source_head");
                self.base_head = str_field(&event.body, "base_head");
                self.merge_base = str_field(&event.body, "merge_base");
            }
            EventKind::PrComment => {
                if let Some(body) = str_field(&event.body, "body") {
                    self.comments.push(body);
                }
            }
            EventKind::PrReview => {
                if let Some(decision) = str_field(&event.body, "decision") {
                    self.effective_decision = Some(decision);
                }
            }
            EventKind::PrMerge => {
                self.merge_result = str_field(&event.body, "result_commit");
            }
            EventKind::CiCheck => {
                // The fold keeps the LATEST CI Check: each event overwrites the
                // prior status/plan/actor/ts, so the PR state always exposes
                // the most recent CI outcome (the merge-gate read).
                self.ci_status = str_field(&event.body, "status");
                self.ci_plan = str_field(&event.body, "plan");
                self.ci_actor = Some(event.actor.clone());
                self.ci_ts = Some(event.ts.clone());
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldState {
    pub issue: IssueState,
    pub pr: PrState,
}

pub fn fold(events: &[Event]) -> FoldState {
    let mut state = FoldState::default();
    for event in events {
        match event.entity.as_str() {
            "issue" => {
                state.issue.id = event.entity_id;
                match event.kind {
                    EventKind::IssueCreated => {
                        state.issue.title = event
                            .body
                            .get("title")
                            .and_then(JsonValue::as_str)
                            .map(String::from);
                        state.issue.description = event
                            .body
                            .get("description")
                            .and_then(JsonValue::as_str)
                            .map(String::from);
                        state.issue.labels = labels_from(&event.body);
                        state.issue.open = true;
                    }
                    EventKind::IssueComment => {
                        if let Some(body) = event.body.get("body").and_then(JsonValue::as_str) {
                            state.issue.comments.push(body.to_string());
                        }
                    }
                    EventKind::IssueClose => state.issue.open = false,
                    EventKind::IssueReopen => state.issue.open = true,
                    _ => {}
                }
            }
            "pr" => {
                state.pr.id = event.entity_id;
                state.pr.apply(event);
            }
            _ => {}
        }
    }
    state
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqState {
    pub next: u64,
}

pub fn first_allocation() -> (u64, SeqState) {
    (1, SeqState { next: 2 })
}

/// Extract the `labels` array from an event body. Only string items are kept;
/// a missing/non-array/non-string `labels` field yields an empty vec (lenient
/// forward-compat: unknown or malformed payloads never break the fold).
fn labels_from(body: &HashMap<String, JsonValue>) -> Vec<String> {
    match body.get("labels") {
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Build a `JsonValue::Array` of string items — the single serialization
/// source for label arrays, shared by the CLI and store so `issue.created`
/// and `pr.created` wire payloads never drift.
pub fn json_string_array(items: &[String]) -> JsonValue {
    JsonValue::Array(items.iter().map(|s| JsonValue::String(s.clone())).collect())
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                // Unescaped C0 controls (other than the short forms above) are
                // invalid in JSON; emit the \u00XX form so stored events stay
                // standards-compliant and round-trip through any parser.
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_object(map: &HashMap<String, JsonValue>) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for (k, v) in map {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&json_string(k));
        out.push(':');
        out.push_str(&json_value(v));
    }
    out.push('}');
    out
}

fn json_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => json_string(s),
        JsonValue::Array(items) => {
            let mut out = String::from("[");
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&json_value(item));
            }
            out.push(']');
            out
        }
        JsonValue::Object(m) => json_object(m),
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r' | b'\n')
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> Option<JsonValue> {
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Some(JsonValue::String(self.parse_string()?)),
            b't' => {
                if self.consume_literal("true") {
                    Some(JsonValue::Bool(true))
                } else {
                    None
                }
            }
            b'f' => {
                if self.consume_literal("false") {
                    Some(JsonValue::Bool(false))
                } else {
                    None
                }
            }
            b'n' => {
                if self.consume_literal("null") {
                    Some(JsonValue::Null)
                } else {
                    None
                }
            }
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn consume_literal(&mut self, lit: &str) -> bool {
        self.skip_ws();
        if self.bytes.get(self.pos..self.pos + lit.len()) == Some(lit.as_bytes()) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn parse_object(&mut self) -> Option<JsonValue> {
        self.pos += 1; // {
        let mut map = HashMap::new();
        self.skip_ws();
        if self.expect(b'}') {
            return Some(JsonValue::Object(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            if !self.expect(b':') {
                return None;
            }
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            if self.expect(b'}') {
                return Some(JsonValue::Object(map));
            }
            if !self.expect(b',') {
                return None;
            }
        }
    }

    fn parse_array(&mut self) -> Option<JsonValue> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.expect(b']') {
            return Some(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            if self.expect(b']') {
                return Some(JsonValue::Array(items));
            }
            if !self.expect(b',') {
                return None;
            }
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        self.skip_ws();
        if self.peek()? != b'"' {
            return None;
        }
        self.pos += 1;
        // Accumulate raw bytes (including multi-byte UTF-8 from the input) and
        // decode once at the end. Decoding byte-by-byte via `c as char` would
        // turn every non-ASCII byte into a Latin-1 char (mojibake); the final
        // from_utf8 validates the sequence and yields the real string.
        let mut bytes: Vec<u8> = Vec::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b'"' {
                self.pos += 1;
                let s = std::str::from_utf8(&bytes).ok()?;
                return Some(s.to_string());
            }
            if c == b'\\' {
                self.pos += 1;
                let esc = *self.bytes.get(self.pos)?;
                self.pos += 1;
                let decoded: char = match esc {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000C}',
                    b'\\' => '\\',
                    b'"' => '"',
                    b'/' => '/',
                    b'u' => {
                        // \uXXXX — four hex digits, decoded to a Unicode
                        // scalar. JSON \u escapes may encode a UTF-16 surrogate
                        // pair (supplementary-plane chars): a high surrogate
                        // must be immediately followed by \uXXXX low surrogate
                        // and the pair combines into one scalar. A lone
                        // low-surrogate (or a high surrogate not followed by a
                        // low surrogate) is invalid and rejected.
                        let code = self.read_hex4()?;
                        let code = if (0xD800..=0xDBFF).contains(&code) {
                            // The low surrogate must be IMMEDIATELY adjacent
                            // (`\uD83D\uDE00`): JSON allows no whitespace between
                            // the two escapes, so use direct byte access instead
                            // of `peek()` (which skips spaces/tabs/CR/LF).
                            if self.bytes.get(self.pos).copied() != Some(b'\\') {
                                return None;
                            }
                            self.pos += 1;
                            if self.bytes.get(self.pos).copied() != Some(b'u') {
                                return None;
                            }
                            self.pos += 1;
                            let low = self.read_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return None;
                            }
                            0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return None; // lone low surrogate
                        } else {
                            code
                        };
                        char::from_u32(code)?
                    }
                    _ => return None,
                };
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(decoded.encode_utf8(&mut buf).as_bytes());
                continue;
            }
            // A raw C0 control byte inside a string is invalid JSON — JSON
            // requires every U+0000–U+001F character to be escaped. The bytes
            // here are never produced by our serializer (json_string escapes
            // them), so rejecting them enforces wire-contract validity on
            // externally authored events. Escaped forms (`\n`, `\b`, `\u0001`)
            // are decoded above and are unaffected.
            if c < 0x20 {
                return None;
            }
            bytes.push(c);
            self.pos += 1;
        }
        None
    }

    /// Read exactly four hex digits as a `u32` (used by `\u` escapes). Advances
    /// `pos` past them; returns `None` on any malformed/truncated input.
    fn read_hex4(&mut self) -> Option<u32> {
        let end = self.pos + 4;
        if end > self.bytes.len() {
            return None;
        }
        let hex = std::str::from_utf8(&self.bytes[self.pos..end]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        self.pos = end;
        Some(code)
    }

    fn parse_number(&mut self) -> Option<JsonValue> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while self.bytes.get(self.pos).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return None;
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        raw.parse::<i64>().ok().map(JsonValue::Number)
    }
}

fn parse_json_value(input: &str) -> Option<JsonValue> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return None;
    }
    Some(value)
}
#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
