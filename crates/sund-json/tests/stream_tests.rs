//! Integration tests for the sund-json stream driver.

use sund_json::reactor::{NullReactor, Reactor};
use sund_json::types::*;
use sund_json::{Stream, StreamStatus};

#[test]
fn test_feed_all_object() {
    let json = br#"{"key":"value"}"#;
    let mut s = Stream::new();
    let status = s.feed_all(json, &mut NullReactor);
    assert_eq!(status, StreamStatus::Done);
}

#[test]
fn test_feed_all_error() {
    let json = b"";
    let mut s = Stream::new();
    let status = s.feed_all(json, &mut NullReactor);
    assert_eq!(status, StreamStatus::Error);
}

#[test]
fn test_streaming_chunks() {
    let chunk1 = br#"{"name":"tes"#;
    let chunk2 = br#"t","age":30}"#;

    let mut s = Stream::new();

    // First chunk: should need more data.
    let (status, tail) = s.feed(chunk1, false, &mut NullReactor);
    assert_eq!(status, StreamStatus::NeedMore);

    // Build next buffer: tail + chunk2.
    let mut next_buf = tail.to_vec();
    next_buf.extend_from_slice(chunk2);

    // Second chunk (final): should complete.
    let (status, _tail) = s.feed(&next_buf, true, &mut NullReactor);
    assert_eq!(status, StreamStatus::Done);
}

/// Event-collecting reactor for stream tests.
#[derive(Debug)]
struct CollectReactor {
    fields: Vec<String>,
    strings: Vec<String>,
    numbers: Vec<String>,
}

impl CollectReactor {
    fn new() -> Self {
        Self {
            fields: Vec::new(),
            strings: Vec::new(),
            numbers: Vec::new(),
        }
    }
}

impl Reactor for CollectReactor {
    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        self.fields.push(key.raw.as_str().unwrap_or("").to_string());
        PROCEED
    }
    fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
        self.numbers.push(raw.as_str().unwrap_or("").to_string());
        PROCEED
    }
    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        self.strings.push(s.raw.as_str().unwrap_or("").to_string());
        PROCEED
    }
}

#[test]
fn test_streaming_events() {
    let chunk1 = br#"{"name":"hel"#;
    let chunk2 = br#"lo","count":42}"#;

    let mut r = CollectReactor::new();
    let mut s = Stream::new();

    let (status, tail) = s.feed(chunk1, false, &mut r);
    assert_eq!(status, StreamStatus::NeedMore);

    let mut next_buf = tail.to_vec();
    next_buf.extend_from_slice(chunk2);

    let (status, _) = s.feed(&next_buf, true, &mut r);
    assert_eq!(status, StreamStatus::Done);

    assert_eq!(r.fields, vec!["name", "count"]);
    assert_eq!(r.strings, vec!["hello"]);
    assert_eq!(r.numbers, vec!["42"]);
}
