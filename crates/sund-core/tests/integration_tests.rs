//! Integration tests for the sund-core parser kernel.

use sund_core::kernel;
use sund_core::reactor::{NullReactor, Reactor};
use sund_core::types::*;

/// Helper: parse a complete JSON input with the given reactor.
/// Returns the exit_code.
fn parse_all<R: Reactor>(json: &[u8], reactor: &mut R) -> i32 {
    let mut ctx = Ctx::new();
    ctx.set_input_slice(json, true);
    unsafe { kernel::parse(&mut ctx, reactor) };
    ctx.exit_code
}

// ---------------------------------------------------------------------------
// Validate-only (NullReactor)
// ---------------------------------------------------------------------------

#[test]
fn test_empty_object() {
    assert_eq!(parse_all(b"{}", &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_empty_array() {
    assert_eq!(parse_all(b"[]", &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_nested_object() {
    let json = br#"{"a":{"b":1}}"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_nested_array() {
    let json = b"[[1,2],[3,4]]";
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_string_value() {
    let json = br#"{"key":"value"}"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_number_values() {
    let json = br#"{"a":42,"b":-3.14,"c":1e10}"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_keywords() {
    let json = br#"{"a":null,"b":true,"c":false}"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_array_of_strings() {
    let json = br#"["hello","world","foo"]"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_complex_json() {
    let json = br#"{
        "name": "test",
        "version": 42,
        "active": true,
        "data": null,
        "tags": ["a", "b", "c"],
        "nested": {
            "x": 1.5,
            "y": -2.5,
            "items": [{"id": 1}, {"id": 2}]
        }
    }"#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_root_string() {
    let json = br#""hello world""#;
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_root_number() {
    assert_eq!(parse_all(b"42", &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_root_null() {
    assert_eq!(parse_all(b"null", &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_root_true() {
    assert_eq!(parse_all(b"true", &mut NullReactor), ExitCode::Ok as i32);
}

#[test]
fn test_root_false() {
    assert_eq!(parse_all(b"false", &mut NullReactor), ExitCode::Ok as i32);
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn test_trailing_content() {
    let json = b"{}{}";
    assert_eq!(
        parse_all(json, &mut NullReactor),
        ExitCode::ErrTrailing as i32
    );
}

#[test]
fn test_empty_input() {
    let json = b"";
    assert_eq!(parse_all(json, &mut NullReactor), ExitCode::ErrEof as i32);
}

#[test]
fn test_syntax_error() {
    let json = b"{,}";
    assert_eq!(
        parse_all(json, &mut NullReactor),
        ExitCode::ErrSyntax as i32
    );
}

// ---------------------------------------------------------------------------
// Event-collecting reactor
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Event {
    BeginObject,
    EndObject,
    Field(String),
    BeginArray,
    EndArray,
    ArrayElem,
    Null,
    Bool(bool),
    Number(String),
    Str(String),
}

struct CollectReactor {
    events: Vec<Event>,
}

impl CollectReactor {
    fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl Reactor for CollectReactor {
    fn begin_object(&mut self) -> i32 {
        self.events.push(Event::BeginObject);
        PROCEED
    }
    fn end_object(&mut self) -> i32 {
        self.events.push(Event::EndObject);
        PROCEED
    }
    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        let s = key.raw.as_str().unwrap_or("").to_string();
        self.events.push(Event::Field(s));
        PROCEED
    }
    fn begin_array(&mut self) -> i32 {
        self.events.push(Event::BeginArray);
        PROCEED
    }
    fn end_array(&mut self) -> i32 {
        self.events.push(Event::EndArray);
        PROCEED
    }
    fn array_elem(&mut self) -> i32 {
        self.events.push(Event::ArrayElem);
        PROCEED
    }
    fn scalar_null(&mut self) -> i32 {
        self.events.push(Event::Null);
        PROCEED
    }
    fn scalar_bool(&mut self, value: bool) -> i32 {
        self.events.push(Event::Bool(value));
        PROCEED
    }
    fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
        let s = raw.as_str().unwrap_or("").to_string();
        self.events.push(Event::Number(s));
        PROCEED
    }
    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        let sv = s.raw.as_str().unwrap_or("").to_string();
        self.events.push(Event::Str(sv));
        PROCEED
    }
}

#[test]
fn test_event_collection() {
    let json = br#"{"name":"test","age":30}"#;
    let mut r = CollectReactor::new();
    let code = parse_all(json, &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    assert_eq!(
        r.events,
        vec![
            Event::BeginObject,
            Event::Field("name".to_string()),
            Event::Str("test".to_string()),
            Event::Field("age".to_string()),
            Event::Number("30".to_string()),
            Event::EndObject,
        ]
    );
}

#[test]
fn test_array_event_collection() {
    let json = b"[1,true,null]";
    let mut r = CollectReactor::new();
    let code = parse_all(json, &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    assert_eq!(
        r.events,
        vec![
            Event::BeginArray,
            Event::ArrayElem,
            Event::Number("1".to_string()),
            Event::ArrayElem,
            Event::Bool(true),
            Event::ArrayElem,
            Event::Null,
            Event::EndArray,
        ]
    );
}

// ---------------------------------------------------------------------------
// SKIP directive
// ---------------------------------------------------------------------------

struct SkipFieldReactor {
    skip_field: String,
    events: Vec<Event>,
}

impl SkipFieldReactor {
    fn new(skip: &str) -> Self {
        Self {
            skip_field: skip.to_string(),
            events: Vec::new(),
        }
    }
}

impl Reactor for SkipFieldReactor {
    fn begin_object(&mut self) -> i32 {
        self.events.push(Event::BeginObject);
        PROCEED
    }
    fn end_object(&mut self) -> i32 {
        self.events.push(Event::EndObject);
        PROCEED
    }
    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        let s = key.raw.as_str().unwrap_or("").to_string();
        if s == self.skip_field {
            return SKIP;
        }
        self.events.push(Event::Field(s));
        PROCEED
    }
    fn begin_array(&mut self) -> i32 {
        self.events.push(Event::BeginArray);
        PROCEED
    }
    fn end_array(&mut self) -> i32 {
        self.events.push(Event::EndArray);
        PROCEED
    }
    fn array_elem(&mut self) -> i32 {
        self.events.push(Event::ArrayElem);
        PROCEED
    }
    fn scalar_null(&mut self) -> i32 {
        self.events.push(Event::Null);
        PROCEED
    }
    fn scalar_bool(&mut self, v: bool) -> i32 {
        self.events.push(Event::Bool(v));
        PROCEED
    }
    fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
        self.events
            .push(Event::Number(raw.as_str().unwrap_or("").to_string()));
        PROCEED
    }
    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        self.events
            .push(Event::Str(s.raw.as_str().unwrap_or("").to_string()));
        PROCEED
    }
}

#[test]
fn test_skip_field() {
    let json = br#"{"a":1,"b":{"nested":true},"c":3}"#;
    let mut r = SkipFieldReactor::new("b");
    let code = parse_all(json, &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    // "b" and its value {"nested":true} should be skipped
    assert_eq!(
        r.events,
        vec![
            Event::BeginObject,
            Event::Field("a".to_string()),
            Event::Number("1".to_string()),
            Event::Field("c".to_string()),
            Event::Number("3".to_string()),
            Event::EndObject,
        ]
    );
}
