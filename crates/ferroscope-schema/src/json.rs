//! A JSON writer and reader small enough to keep the crate dependency-free.
//!
//! Only what a recording needs: objects, arrays, strings, finite numbers, booleans, null.
//! The reader exists so `ferroscope verify` can recompute a run's trace digest **from the
//! recording itself**, with no access to the process that wrote it. A receipt you can only
//! recompute by re-running the simulator is not a receipt.

use std::fmt::Write as _;

/// Builds a JSON object without allocating a tree first.
pub struct Obj {
    s: String,
    first: bool,
}

impl Obj {
    pub fn new() -> Self {
        Obj {
            s: String::from("{"),
            first: true,
        }
    }
    fn comma(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.s.push(',');
        }
    }
    pub fn str(mut self, k: &str, v: &str) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        write_string(&mut self.s, v);
        self
    }
    pub fn num(mut self, k: &str, v: f64) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        write_number(&mut self.s, v);
        self
    }
    pub fn int(mut self, k: &str, v: i64) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        let _ = write!(self.s, "{v}");
        self
    }
    pub fn uint(mut self, k: &str, v: u64) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        let _ = write!(self.s, "{v}");
        self
    }
    pub fn nums(mut self, k: &str, v: &[f64]) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        self.s.push('[');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.s.push(',');
            }
            write_number(&mut self.s, *x);
        }
        self.s.push(']');
        self
    }
    pub fn strs(mut self, k: &str, v: &[String]) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        self.s.push('[');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.s.push(',');
            }
            write_string(&mut self.s, x);
        }
        self.s.push(']');
        self
    }
    pub fn raw(mut self, k: &str, v: &str) -> Self {
        self.comma();
        write_key(&mut self.s, k);
        self.s.push_str(v);
        self
    }
    /// Finish as a parsed [`Value`], for a caller that wants to keep building around it.
    pub fn finish_value(self) -> Value {
        parse(&self.finish()).unwrap_or(Value::Null)
    }

    pub fn finish(mut self) -> String {
        self.s.push('}');
        self.s
    }
}

impl Default for Obj {
    fn default() -> Self {
        Self::new()
    }
}

fn write_key(out: &mut String, k: &str) {
    write_string(out, k);
    out.push(':');
}

pub fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// JSON has no NaN and no infinity. Emitting `null` instead of inventing a token keeps the
/// file readable by every other tool; the *count* of non-finite values is what the receipt
/// carries, so nothing is quietly lost.
pub fn write_number(out: &mut String, v: f64) {
    if v.is_finite() {
        if v == v.trunc() && v.abs() < 1e15 && !(v == 0.0 && v.is_sign_negative()) {
            let _ = write!(out, "{}", v as i64);
        } else {
            // Debug, not Display: `{:?}` is the shortest form that round-trips, and it uses
            // an exponent for extremes instead of writing 300 zeros into the recording.
            let _ = write!(out, "{v:?}");
        }
    } else {
        out.push_str("null");
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Null => Some(f64::NAN),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
    /// Every number in the value, in document order — the trace digest's view of a payload.
    pub fn numbers(&self, out: &mut Vec<f64>) {
        match self {
            Value::Num(n) => out.push(*n),
            Value::Null => {}
            Value::Arr(a) => a.iter().for_each(|v| v.numbers(out)),
            Value::Obj(kv) => kv.iter().for_each(|(_, v)| v.numbers(out)),
            _ => {}
        }
    }
}

impl Value {
    /// Serialize back to JSON text. Round-trips through [`parse`].
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => write_number(out, *n),
            Value::Str(s) => write_string(out, s),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(kv) => {
                out.push('{');
                for (i, (k, v)) in kv.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Follow a dotted path, for the `@` predicate operator.
    pub fn path(&self, dotted: &str) -> Option<&Value> {
        let mut cur = self;
        for seg in dotted.split('.') {
            cur = cur.get(seg)?;
        }
        Some(cur)
    }

    /// A short human rendering for a table cell.
    pub fn brief(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Num(n) => {
                let mut o = String::new();
                write_number(&mut o, *n);
                o
            }
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".into(),
            other => other.to_json(),
        }
    }
}

/// Parse a JSON document. Returns `None` on anything malformed — a recording with a broken
/// payload should be reported, not half-read.
pub fn parse(s: &str) -> Option<Value> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let v = parse_value(b, &mut i)?;
    skip_ws(b, &mut i);
    if i == b.len() {
        Some(v)
    } else {
        None
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn parse_value(b: &[u8], i: &mut usize) -> Option<Value> {
    skip_ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut kv = Vec::new();
            skip_ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Some(Value::Obj(kv));
            }
            loop {
                skip_ws(b, i);
                let k = parse_string(b, i)?;
                skip_ws(b, i);
                if b.get(*i)? != &b':' {
                    return None;
                }
                *i += 1;
                let v = parse_value(b, i)?;
                kv.push((k, v));
                skip_ws(b, i);
                match *b.get(*i)? {
                    b',' => *i += 1,
                    b'}' => {
                        *i += 1;
                        return Some(Value::Obj(kv));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut a = Vec::new();
            skip_ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Some(Value::Arr(a));
            }
            loop {
                a.push(parse_value(b, i)?);
                skip_ws(b, i);
                match *b.get(*i)? {
                    b',' => *i += 1,
                    b']' => {
                        *i += 1;
                        return Some(Value::Arr(a));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => Some(Value::Str(parse_string(b, i)?)),
        b't' => lit(b, i, b"true").map(|_| Value::Bool(true)),
        b'f' => lit(b, i, b"false").map(|_| Value::Bool(false)),
        b'n' => lit(b, i, b"null").map(|_| Value::Null),
        _ => parse_number(b, i),
    }
}

fn lit(b: &[u8], i: &mut usize, want: &[u8]) -> Option<()> {
    if b.len() >= *i + want.len() && &b[*i..*i + want.len()] == want {
        *i += want.len();
        Some(())
    } else {
        None
    }
}

fn parse_string(b: &[u8], i: &mut usize) -> Option<String> {
    if *b.get(*i)? != b'"' {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    loop {
        match *b.get(*i)? {
            b'"' => {
                *i += 1;
                return Some(out);
            }
            b'\\' => {
                *i += 1;
                match *b.get(*i)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let hex = std::str::from_utf8(b.get(*i + 1..*i + 5)?).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(cp)?);
                        *i += 4;
                    }
                    _ => return None,
                }
                *i += 1;
            }
            _ => {
                // Copy one UTF-8 scalar without re-validating byte by byte.
                let start = *i;
                let len = utf8_len(b[*i]);
                *i += len;
                out.push_str(std::str::from_utf8(b.get(start..*i)?).ok()?);
            }
        }
    }
}

fn utf8_len(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn parse_number(b: &[u8], i: &mut usize) -> Option<Value> {
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    while matches!(b.get(*i), Some(c) if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    std::str::from_utf8(&b[start..*i])
        .ok()?
        .parse::<f64>()
        .ok()
        .map(Value::Num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_payload() {
        let s = Obj::new()
            .str("frame", "world")
            .nums("t", &[1.0, -2.5, 0.0])
            .uint("step", 42)
            .finish();
        let v = parse(&s).expect("parse");
        assert_eq!(v.get("frame").unwrap().as_str(), Some("world"));
        let mut n = Vec::new();
        v.numbers(&mut n);
        assert_eq!(n, vec![1.0, -2.5, 0.0, 42.0]);
    }

    #[test]
    fn non_finite_becomes_null_not_a_bogus_token() {
        let s = Obj::new()
            .num("x", f64::NAN)
            .num("y", f64::INFINITY)
            .finish();
        assert_eq!(s, r#"{"x":null,"y":null}"#);
        assert!(parse(&s).is_some(), "the file stays valid JSON");
    }

    #[test]
    fn escapes_survive() {
        let s = Obj::new().str("k", "a\"b\\c\nd\te").finish();
        let v = parse(&s).unwrap();
        assert_eq!(v.get("k").unwrap().as_str(), Some("a\"b\\c\nd\te"));
    }

    #[test]
    fn unicode_survives() {
        let s = Obj::new().str("k", "Φ · joules ✓").finish();
        let v = parse(&s).unwrap();
        assert_eq!(v.get("k").unwrap().as_str(), Some("Φ · joules ✓"));
    }

    #[test]
    fn malformed_is_rejected_not_half_read() {
        assert!(parse("{\"a\":1,}").is_none());
        assert!(parse("[1,2").is_none());
        assert!(parse("{\"a\":1} trailing").is_none());
    }

    #[test]
    fn floats_round_trip_exactly() {
        for v in [0.1, 1.0 / 3.0, 1e-300, 6.02214076e23, -2.5e-8] {
            let s = Obj::new().num("v", v).finish();
            let back = parse(&s).unwrap().get("v").unwrap().as_f64().unwrap();
            assert_eq!(back.to_bits(), v.to_bits(), "{v} did not round-trip");
        }
    }
}
