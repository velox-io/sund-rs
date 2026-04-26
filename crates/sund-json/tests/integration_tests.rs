//! Integration tests for the sund-json parser.

use sund_json::parser;
use sund_json::reactor::{NullReactor, Reactor};
use sund_json::types::*;

/// Helper: parse a complete JSON input with the given reactor.
/// Returns the exit_code.
fn parse_all<R: Reactor>(json: &[u8], reactor: &mut R) -> i32 {
    let mut ctx = Ctx::new();
    ctx.set_input_slice(json, true);
    unsafe { parser::parse(&mut ctx, reactor) };
    ctx.exit_code
}

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

// ================================================================
// Long-string / multi-chunk tests
//
// Each 64-byte SIMD chunk produces a structural bitmap.  Strings
// longer than ~60 bytes necessarily span multiple chunks, exercising
// the `string_span` → `advance_chunk_outlined` path, the cross-chunk
// `bs_bits` / `has_escape` tracking, and the `ctz64_empty` helper
// that decides whether the current bitmap is exhausted.
// ================================================================

/// Build a JSON string literal of exactly `n` content bytes (plus quotes).
/// Content is 'a' repeated `n` times.  Total payload: `n + 2` bytes.
fn make_long_string(n: usize) -> String {
    let mut s = String::with_capacity(n + 2);
    s.push('"');
    for _ in 0..n {
        s.push('a');
    }
    s.push('"');
    s
}

/// Build a JSON string literal of `n` content bytes that include
/// backslash escapes at specified offsets (relative to string start).
/// `esc_offsets` specifies positions where `\\n` (two-byte escape)
/// should be placed.  Remaining positions are filled with 'a'.
fn make_escaped_string(n: usize, esc_offsets: &[usize]) -> String {
    let mut content = vec![b'a'; n];
    for &off in esc_offsets {
        if off + 1 < n {
            content[off] = b'\\';
            content[off + 1] = b'n';
        }
    }
    let mut s = String::with_capacity(n + 2);
    s.push('"');
    s.push_str(std::str::from_utf8(&content).unwrap());
    s.push('"');
    s
}

// ---------- Single-shot tests for long strings ----------

#[test]
fn test_long_string_150_bytes_as_value() {
    // 150-byte string value, spans 3 chunks: {" at byte 7, content 8..157, "} at 158
    let val = make_long_string(150);
    let json = format!(r#"{{"key":{}}}"#, val);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    assert_eq!(r.events.len(), 4); // BeginObj, Field, Str, EndObj
    match &r.events[2] {
        Event::Str(s) => assert_eq!(s.len(), 150),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn test_long_string_150_bytes_as_key() {
    let key_content: String = std::iter::repeat('k').take(150).collect();
    let json = format!(r#"{{"{}":{}}}"#, key_content, 42);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[1] {
        Event::Field(s) => {
            assert_eq!(s.len(), 150);
            assert!(s.chars().all(|c| c == 'k'));
        }
        other => panic!("expected Field, got {:?}", other),
    }
}

#[test]
fn test_long_string_150_bytes_in_array() {
    let val = make_long_string(150);
    let json = format!("[{}]", val);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    // BeginArray, ArrayElem, Str, EndArray
    match &r.events[2] {
        Event::Str(s) => assert_eq!(s.len(), 150),
        other => panic!("expected Str, got {:?}", other),
    }
}

/// Sweep string lengths from 1 to 256 to catch any chunk-boundary edge cases.
#[test]
fn test_string_length_sweep_1_to_256() {
    for n in 1..=256 {
        let val = make_long_string(n);
        let json = format!("[{}]", val);
        let mut r = CollectReactor::new();
        let code = parse_all(json.as_bytes(), &mut r);
        assert_eq!(
            code,
            ExitCode::Ok as i32,
            "failed for string length {}",
            n
        );
        match &r.events[2] {
            Event::Str(s) => assert_eq!(
                s.len(),
                n,
                "string content length mismatch at n={}",
                n
            ),
            other => panic!("expected Str at n={}, got {:?}", n, other),
        }
    }
}

/// Sweep key lengths from 1 to 256 as object field keys.
#[test]
fn test_key_length_sweep_1_to_256() {
    for n in 1..=256 {
        let key_content: String = std::iter::repeat('x').take(n).collect();
        let json = format!(r#"{{"{}":{}}}"#, key_content, 1);
        let mut r = CollectReactor::new();
        let code = parse_all(json.as_bytes(), &mut r);
        assert_eq!(
            code,
            ExitCode::Ok as i32,
            "failed for key length {}",
            n
        );
        match &r.events[1] {
            Event::Field(s) => assert_eq!(
                s.len(),
                n,
                "key content length mismatch at n={}",
                n
            ),
            other => panic!("expected Field at n={}, got {:?}", n, other),
        }
    }
}

// ---------- Escape handling across chunk boundaries ----------

#[test]
fn test_long_string_with_escape_at_chunk_boundary() {
    // Place a backslash escape right at byte 63 (end of first chunk).
    // The opening " is at some offset, so the escape position relative
    // to string start depends on the JSON prefix.
    // JSON: {"k":"...\\n..."}
    // Prefix `{"k":"` = 6 bytes, so string content starts at byte 6.
    // We want escape at absolute position 63, so content offset = 63-6 = 57.
    let esc = make_escaped_string(150, &[57]);
    let json = format!(r#"{{"k":{}}}"#, esc);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[2] {
        Event::Str(s) => {
            assert_eq!(s.len(), 150);
            assert!(s.contains("\\n"), "should contain escape sequence");
        }
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn test_long_string_with_escape_at_chunk_boundary_64() {
    // Place a backslash escape right at byte 64 (start of second chunk).
    // Prefix `{"k":"` = 6 bytes, content offset = 64-6 = 58.
    let esc = make_escaped_string(150, &[58]);
    let json = format!(r#"{{"k":{}}}"#, esc);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[2] {
        Event::Str(s) => assert_eq!(s.len(), 150),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn test_long_string_with_multiple_escapes_across_chunks() {
    // Escapes at chunk boundaries and mid-chunk positions.
    // Prefix is 6 bytes, so content byte at offset i maps to absolute
    // position 6+i.  We put escapes near each 64-byte boundary.
    let esc = make_escaped_string(200, &[50, 56, 57, 58, 120, 121, 122]);
    let json = format!(r#"{{"k":{}}}"#, esc);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[2] {
        Event::Str(s) => assert_eq!(s.len(), 200),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn test_escaped_quote_inside_long_string() {
    // A \" inside a long string must NOT terminate the string.
    let mut content = vec![b'a'; 150];
    content[70] = b'\\';
    content[71] = b'"';
    let mut json_bytes = Vec::new();
    json_bytes.extend_from_slice(b"[\"");
    json_bytes.extend_from_slice(&content);
    json_bytes.extend_from_slice(b"\"]");
    let mut r = CollectReactor::new();
    let code = parse_all(&json_bytes, &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[2] {
        Event::Str(s) => {
            assert_eq!(s.len(), 150, "escaped quote should not truncate the string");
        }
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn test_escaped_quote_at_chunk_boundary() {
    // Put \" right at the 64-byte boundary inside a string.
    // The string starts at byte 2 (["), so content byte 62 is at
    // absolute position 64.
    let mut content = vec![b'a'; 150];
    // Position 61,62 in content → absolute 63,64 → straddles chunk boundary
    content[61] = b'\\';
    content[62] = b'"';
    let mut json_bytes = Vec::new();
    json_bytes.extend_from_slice(b"[\"");
    json_bytes.extend_from_slice(&content);
    json_bytes.extend_from_slice(b"\"]");
    let mut r = CollectReactor::new();
    let code = parse_all(&json_bytes, &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    match &r.events[2] {
        Event::Str(s) => {
            assert_eq!(s.len(), 150, "escaped quote at chunk boundary should not truncate");
        }
        other => panic!("expected Str, got {:?}", other),
    }
}

// ---------- Multiple long strings ----------

#[test]
fn test_multiple_long_strings_in_object() {
    let v1 = make_long_string(100);
    let v2 = make_long_string(150);
    let v3 = make_long_string(200);
    let json = format!(r#"{{"a":{},"b":{},"c":{}}}"#, v1, v2, v3);
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    let strs: Vec<&str> = r.events.iter().filter_map(|e| match e {
        Event::Str(s) => Some(s.as_str()),
        _ => None,
    }).collect();
    assert_eq!(strs.len(), 3);
    assert_eq!(strs[0].len(), 100);
    assert_eq!(strs[1].len(), 150);
    assert_eq!(strs[2].len(), 200);
}

#[test]
fn test_multiple_long_strings_in_array() {
    let vals: Vec<String> = (60..=200).step_by(10).map(|n| make_long_string(n)).collect();
    let json = format!("[{}]", vals.join(","));
    let mut r = CollectReactor::new();
    let code = parse_all(json.as_bytes(), &mut r);
    assert_eq!(code, ExitCode::Ok as i32);
    let strs: Vec<usize> = r.events.iter().filter_map(|e| match e {
        Event::Str(s) => Some(s.len()),
        _ => None,
    }).collect();
    let expected: Vec<usize> = (60..=200).step_by(10).collect();
    assert_eq!(strs, expected);
}

// ---------- String ending exactly at chunk boundary ----------

#[test]
fn test_string_ending_at_chunk_boundary() {
    // Try to make the closing " land exactly at byte 63, 64, 127, 128.
    for target_close in [63usize, 64, 127, 128] {
        // The string starts with [", so content_start = 2.
        // We need content_start + content_len + 1 (closing ") = target_close + 1
        // content_len = target_close + 1 - 2 - 1 = target_close - 2
        if target_close < 3 {
            continue;
        }
        let content_len = target_close - 2;
        let val = make_long_string(content_len);
        let json = format!("[{}]", val);
        let mut r = CollectReactor::new();
        let code = parse_all(json.as_bytes(), &mut r);
        assert_eq!(
            code,
            ExitCode::Ok as i32,
            "failed when closing quote targets byte {}",
            target_close
        );
        match &r.events[2] {
            Event::Str(s) => assert_eq!(
                s.len(),
                content_len,
                "length mismatch when closing quote targets byte {}",
                target_close
            ),
            other => panic!(
                "expected Str when closing quote targets byte {}, got {:?}",
                target_close, other
            ),
        }
    }
}

// ---------- Long string with ndec parity check ----------

/// Parse JSON and collect string values + has_escape flag.
struct EscapeCheckReactor {
    strings: Vec<(String, bool)>,
}

impl EscapeCheckReactor {
    fn new() -> Self {
        Self { strings: Vec::new() }
    }
}

impl Reactor for EscapeCheckReactor {
    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        let sv = s.raw.as_str().unwrap_or("").to_string();
        self.strings.push((sv, s.has_escape));
        PROCEED
    }
    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        // Don't record keys, just values.
        let _ = key;
        PROCEED
    }
}

#[test]
fn test_long_string_has_escape_flag() {
    // No escape: has_escape should be false.
    let val_no_esc = make_long_string(150);
    let json = format!("[{}]", val_no_esc);
    let mut r = EscapeCheckReactor::new();
    parse_all(json.as_bytes(), &mut r);
    assert_eq!(r.strings.len(), 1);
    assert!(!r.strings[0].1, "150-byte string without escapes should have has_escape=false");

    // With escape: has_escape should be true.
    let val_esc = make_escaped_string(150, &[75]);
    let json = format!("[{}]", val_esc);
    let mut r = EscapeCheckReactor::new();
    parse_all(json.as_bytes(), &mut r);
    assert_eq!(r.strings.len(), 1);
    assert!(r.strings[0].1, "150-byte string with escape should have has_escape=true");
}

#[test]
fn test_long_string_has_escape_across_chunks() {
    // Escape only in the second chunk (byte 70 of content, after the
    // 64-byte boundary).  has_escape must still be true.
    let esc = make_escaped_string(150, &[80]);
    let json = format!(r#"[{}]"#, esc);
    let mut r = EscapeCheckReactor::new();
    parse_all(json.as_bytes(), &mut r);
    assert!(r.strings[0].1, "escape in second chunk should set has_escape");
}

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
