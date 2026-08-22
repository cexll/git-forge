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
            ts: "1970-01-01T00:00:00Z".to_string(),
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
            ts: "1970-01-01T00:00:00Z".to_string(),
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
    pub comments: Vec<String>,
    pub open: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub source_ref: Option<String>,
    pub base_ref: Option<String>,
    pub source_head: Option<String>,
    pub base_head: Option<String>,
    pub merge_base: Option<String>,
    pub comments: Vec<String>,
    pub effective_decision: Option<String>,
    pub merge_result: Option<String>,
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

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
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
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b'"' {
                self.pos += 1;
                return Some(out);
            }
            if c == b'\\' {
                self.pos += 1;
                let esc = *self.bytes.get(self.pos)?;
                self.pos += 1;
                out.push(match esc {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000C}',
                    b'\\' => '\\',
                    b'"' => '"',
                    b'/' => '/',
                    _ => return None,
                });
                continue;
            }
            out.push(c as char);
            self.pos += 1;
        }
        None
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
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn body_with(kv: Vec<(&str, JsonValue)>) -> HashMap<String, JsonValue> {
        kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    /// v1 schema serialization is self-consistent: to_json -> from_json is the
    /// identity on every field, including control characters that must be
    /// escaped in the wire encoding (wire contract § Event JSON schema v1).
    #[test]
    fn json_roundtrip_escapes_control_chars_in_actor_and_body() {
        let ev = Event::new(
            EventKind::IssueComment,
            "issue",
            7,
            "dev@example.com",
            body_with(vec![
                ("title", JsonValue::String("a\"b\\c\nd\te\rf".into())),
                ("n", JsonValue::Number(-42)),
                ("ok", JsonValue::Bool(true)),
                ("none", JsonValue::Null),
            ]),
        );
        let json = ev.to_json();
        assert!(json.contains("\\\"") && json.contains("\\\\") && json.contains("\\n"));
        let round = Event::from_json(&json).expect("roundtrip must parse");
        assert_eq!(round, ev);
    }

    /// Nested arrays/objects survive serialization and parsing verbatim.
    #[test]
    fn json_roundtrip_nested_structures() {
        let nested = JsonValue::Array(vec![
            JsonValue::Object(body_with(vec![(
                "deep",
                JsonValue::Array(vec![JsonValue::Number(1), JsonValue::String("x".into())]),
            )])),
            JsonValue::Number(0),
        ]);
        let ev = Event::new(
            EventKind::PrReview,
            "pr",
            3,
            "a@b.c",
            body_with(vec![("review", nested)]),
        );
        let round = Event::from_json(&ev.to_json()).expect("nested roundtrip must parse");
        assert_eq!(round, ev);
    }

    /// from_json is strict: malformed shape, wrong schema version, non-UUID id,
    /// unknown kind, or a non-object body must all be rejected, never coerced.
    #[test]
    fn from_json_rejects_malformed_shapes() {
        let good =
            Event::new(EventKind::IssueCreated, "issue", 1, "x@y.z", HashMap::new()).to_json();
        // Truncate the JSON to a malformed but parseable-then-absent tail.
        assert!(Event::from_json("not json").is_none());
        // strip the closing brace -> trailing garbage, parse must reject.
        assert!(Event::from_json(&good[..good.len() - 1]).is_none());
        // wrong schema version
        let wrong_v = good.replace("\"v\":1", "\"v\":2");
        assert!(Event::from_json(&wrong_v).is_none());
        // non-object body
        let bad_body = good.replace("\"body\":{", "\"body\":[");
        assert!(Event::from_json(&bad_body).is_none());
    }

    /// EventKind as_str/from_str roundtrip for every wire kind; unknown rejects.
    #[test]
    fn event_kind_string_roundtrip_all_kinds() {
        for kind in [
            EventKind::IssueCreated,
            EventKind::IssueComment,
            EventKind::IssueClose,
            EventKind::IssueReopen,
            EventKind::PrCreated,
            EventKind::PrComment,
            EventKind::PrReview,
            EventKind::PrMerge,
        ] {
            assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), kind);
        }
        assert!("pr.merge".parse::<EventKind>().is_ok());
        assert!("issue.bogus".parse::<EventKind>().is_err());
        assert!("".parse::<EventKind>().is_err());
    }

    /// UUID-v4 shape validation: exact length/hyphen positions, version nibble,
    /// variant nibble, and hex-only rejection.
    #[test]
    fn uuid_v4_shape_boundaries() {
        assert!(is_uuid_v4("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_uuid_v4("00000000-0000-4000-8000-000000000000"));
        // wrong length
        assert!(!is_uuid_v4("550e8400-e29b-41d4-a716-44665544000"));
        // wrong hyphen position
        assert!(!is_uuid_v4("550e8400-e29b-41d4-a716446655440000"));
        // wrong version nibble (not 4)
        assert!(!is_uuid_v4("550e8400-e29b-51d4-a716-446655440000"));
        // wrong variant nibble (not 8/9/a/b)
        assert!(!is_uuid_v4("550e8400-e29b-41d4-1716-446655440000"));
        // non-hex char
        assert!(!is_uuid_v4("550e8400-e29b-41d4-a716-44665544000g"));
    }
}
