//! **A `ros2 bag` is numbers, if you can read its definition.**
//!
//! An MCAP written by ROS 2 carries the FULL message definition inline, as `ros2msg` text, beside
//! payloads encoded in CDR. That makes the recording self-describing: everything needed to turn
//! `/joint_states` into plottable numbers is in the file, and none of it needs ROS installed, a
//! workspace sourced, or a message package built.
//!
//! This crate is that reader, in `std` alone — because the point is a browser tab opening a robot
//! log, and a ROS client library would end that immediately.
//!
//! ```
//! use ferroscope_ros2::MessageDef;
//!
//! let def = MessageDef::parse("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n").unwrap();
//! // A little-endian CDR payload: sec = 2, nanosec = 5.
//! let payload = [0, 1, 0, 0,  2, 0, 0, 0,  5, 0, 0, 0];
//! assert_eq!(def.decode_numbers(&payload).unwrap(), vec![2.0, 5.0]);
//! ```
//!
//! # What it does not do
//!
//! Decode to a typed struct — the output is numbers in field order, which is what a plot, a
//! digest and a comparison need. Strings are consumed and skipped rather than returned.
//! Parameter-list CDR (`0x0002`/`0x0003`, used by DDS for keyed topics) is refused rather than
//! guessed at, as are `wstring` fields.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

mod cdr;
pub use cdr::Cdr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The payload ran out mid-field.
    Short { want: usize, have: usize },
    /// Bytes remained after the last field: the definition and the payload disagree.
    Trailing { left: usize, total: usize },
    /// An encapsulation this decoder will not guess at.
    Encapsulation(u16),
    /// A string field was not valid UTF-8.
    BadUtf8,
    /// A field referenced a message type the definition bundle does not contain.
    UnknownType { field: String, ty: String },
    /// A type this decoder does not implement, named rather than skipped.
    Unsupported(String),
    /// A field line that is not `<type> <name>`.
    BadField(String),
    /// Nested types refer to each other in a cycle.
    TooDeep,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Short { want, have } => {
                write!(f, "CDR payload ran out: wanted {want} bytes, {have} remain")
            }
            Error::Trailing { left, total } => write!(
                f,
                "{left} of {total} bytes were never read: the message definition does not match \
                 the payload"
            ),
            Error::Encapsulation(id) => write!(
                f,
                "CDR encapsulation {id:#06x} is not plain CDR; this reader does not guess at \
                 parameter-list payloads"
            ),
            Error::BadUtf8 => write!(f, "a string field was not valid UTF-8"),
            Error::UnknownType { field, ty } => {
                write!(
                    f,
                    "field `{field}` has type `{ty}`, which the definition bundle does not \
                          define"
                )
            }
            Error::Unsupported(t) => write!(f, "`{t}` fields are not decoded by this reader"),
            Error::BadField(l) => write!(f, "not a field declaration: `{l}`"),
            Error::TooDeep => write!(f, "message definitions nest into a cycle"),
        }
    }
}

impl std::error::Error for Error {}

/// A decoded message, with its structure and its strings intact.
///
/// The first version of this decoder emitted a flat `Vec<f64>` and threw strings away, which is
/// all a plot or a digest needs. It is not enough to place a robot: `tf2_msgs/TFMessage` says
/// which frame moved in a `child_frame_id` STRING, and a transform without its frame is a row of
/// numbers with nowhere to go. So the decode produces a tree, and the numeric views are walks
/// over it -- one decoder, two views, rather than two decoders that can disagree.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
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

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// Every number, in field order. Strings contribute nothing, exactly as before.
    pub fn numbers(&self, out: &mut Vec<f64>) {
        match self {
            Value::Num(n) => out.push(*n),
            Value::Arr(a) => a.iter().for_each(|v| v.numbers(out)),
            Value::Obj(kv) => kv.iter().for_each(|(_, v)| v.numbers(out)),
            Value::Str(_) => {}
        }
    }

    /// Every number with the path that reaches it: `header.stamp.sec`, `position[1]`.
    pub fn labeled(&self, prefix: &str, out: &mut Vec<(String, f64)>) {
        match self {
            Value::Num(n) => out.push((prefix.to_string(), *n)),
            Value::Arr(a) => {
                for (i, v) in a.iter().enumerate() {
                    v.labeled(&format!("{prefix}[{i}]"), out);
                }
            }
            Value::Obj(kv) => {
                for (k, v) in kv {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    v.labeled(&path, out);
                }
            }
            Value::Str(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Prim(Ty),
    Msg(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    One,
    /// `T[N]` — no count on the wire.
    Fixed(usize),
    /// `T[]` and `T[<=N]` — a `u32` count, then the elements.
    Seq,
}

#[derive(Debug, Clone)]
struct Field {
    name: String,
    kind: Kind,
    arity: Arity,
}

/// One message type and every type it refers to, parsed from an MCAP schema's `ros2msg` text.
#[derive(Debug, Clone)]
pub struct MessageDef {
    root: String,
    types: BTreeMap<String, Vec<Field>>,
}

/// `sensor_msgs/msg/JointState` and `sensor_msgs/JointState` name the same type. Definitions and
/// field declarations do not agree about which spelling to use, so everything is keyed on the
/// short one.
fn normalize(name: &str) -> String {
    let parts: Vec<&str> = name.split('/').filter(|p| *p != "msg").collect();
    parts.join("/")
}

fn primitive(t: &str) -> Option<Ty> {
    Some(match t {
        "bool" => Ty::Bool,
        // `byte` and `char` are the deprecated spellings of uint8 and uint8.
        "byte" | "uint8" | "char" => Ty::U8,
        "int8" => Ty::I8,
        "uint16" => Ty::U16,
        "int16" => Ty::I16,
        "uint32" => Ty::U32,
        "int32" => Ty::I32,
        "uint64" => Ty::U64,
        "int64" => Ty::I64,
        "float32" => Ty::F32,
        "float64" => Ty::F64,
        _ => return None,
    })
}

impl MessageDef {
    /// Parse a schema's definition text. `root` is the schema name from the MCAP record.
    ///
    /// The bundle is the root type's fields, then each referenced type after a line of `=`
    /// followed by `MSG: <name>`.
    pub fn parse(root: &str, text: &str) -> Result<Self, Error> {
        let mut types: BTreeMap<String, Vec<Field>> = BTreeMap::new();
        let root_norm = normalize(root);
        let mut current = root_norm.clone();
        let mut fields: Vec<Field> = Vec::new();

        for raw in text.lines() {
            // A comment runs to the end of the line. Constants may carry a string default with a
            // `#` in it; those lines are dropped anyway, so this is safe here and noted as a
            // limitation rather than hidden.
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.chars().all(|c| c == '=') {
                continue; // the separator rule
            }
            if let Some(name) = line.strip_prefix("MSG:") {
                types.insert(std::mem::take(&mut current), std::mem::take(&mut fields));
                current = normalize(name.trim());
                continue;
            }
            // A constant, not a field: `uint8 STATE_OK=1`. Not serialized.
            if line.contains('=') {
                continue;
            }
            let mut it = line.split_whitespace();
            let (Some(tyname), Some(fname)) = (it.next(), it.next()) else {
                return Err(Error::BadField(line.to_string()));
            };
            fields.push(parse_field(tyname, fname)?);
        }
        types.insert(current, fields);
        Ok(Self {
            root: root_norm,
            types,
        })
    }

    /// The numbers in this message, in field order.
    pub fn decode_numbers(&self, payload: &[u8]) -> Result<Vec<f64>, Error> {
        let mut out = Vec::new();
        self.decode(payload)?.numbers(&mut out);
        Ok(out)
    }

    /// The numbers, and a name for each — `header.stamp.sec`, `position[1]`.
    ///
    /// The comparator reports `channel[4]`, and `[4]` is not a name. A ROS 2 recording carries
    /// the field names in the file, so there is no reason to print an index.
    pub fn decode_labeled(&self, payload: &[u8]) -> Result<(Vec<f64>, Vec<String>), Error> {
        let mut pairs = Vec::new();
        self.decode(payload)?.labeled("", &mut pairs);
        // `labeled` yields (name, value); the caller wants values first.
        Ok(pairs.into_iter().map(|(n, v)| (v, n)).unzip())
    }

    /// The message as a tree, with its strings.
    pub fn decode(&self, payload: &[u8]) -> Result<Value, Error> {
        let mut c = Cdr::new(payload)?;
        let v = self.read_msg(&self.root, &mut c, 0)?;
        c.finish()?;
        Ok(v)
    }

    fn read_msg(&self, ty: &str, c: &mut Cdr<'_>, depth: u32) -> Result<Value, Error> {
        if depth > 32 {
            return Err(Error::TooDeep);
        }
        let fields = self.types.get(ty).ok_or_else(|| Error::UnknownType {
            field: String::new(),
            ty: ty.to_string(),
        })?;
        let mut obj: Vec<(String, Value)> = Vec::with_capacity(fields.len());
        for f in fields {
            let n = match f.arity {
                Arity::One => 1,
                Arity::Fixed(n) => n,
                Arity::Seq => c.u32()? as usize,
            };
            let mut elems: Vec<Value> = Vec::new();
            for _ in 0..n {
                elems.push(match &f.kind {
                    Kind::Prim(Ty::Str) => Value::Str(c.string()?.to_string()),
                    Kind::Prim(p) => Value::Num(read_prim(*p, c)?),
                    Kind::Msg(m) => {
                        let resolved = self.resolve(m, ty).ok_or_else(|| Error::UnknownType {
                            field: f.name.clone(),
                            ty: m.clone(),
                        })?;
                        self.read_msg(&resolved, c, depth + 1)?
                    }
                });
            }
            let v = if matches!(f.arity, Arity::One) {
                elems.pop().expect("arity One reads exactly one element")
            } else {
                Value::Arr(elems)
            };
            obj.push((f.name.clone(), v));
        }
        Ok(Value::Obj(obj))
    }

    /// A field may name a type fully (`std_msgs/Header`) or bare (`Header`), in which case it
    /// means the package the referring type came from.
    fn resolve(&self, name: &str, from: &str) -> Option<String> {
        let n = normalize(name);
        if self.types.contains_key(&n) {
            return Some(n);
        }
        if !n.contains('/')
            && let Some(pkg) = from.split('/').next()
        {
            let q = format!("{pkg}/{n}");
            if self.types.contains_key(&q) {
                return Some(q);
            }
        }
        // Last resort: a unique type whose short name matches.
        let mut hit = None;
        for k in self.types.keys() {
            if k.rsplit('/').next() == Some(n.rsplit('/').next().unwrap_or(&n)) {
                if hit.is_some() {
                    return None; // ambiguous: refuse rather than pick
                }
                hit = Some(k.clone());
            }
        }
        hit
    }
}

fn read_prim(t: Ty, c: &mut Cdr<'_>) -> Result<f64, Error> {
    Ok(match t {
        Ty::Bool | Ty::U8 => f64::from(c.u8()?),
        Ty::I8 => f64::from(c.i8()?),
        Ty::U16 => f64::from(c.u16()?),
        Ty::I16 => f64::from(c.i16()?),
        Ty::U32 => f64::from(c.u32()?),
        Ty::I32 => f64::from(c.i32()?),
        // A u64/i64 beyond 2^53 does not survive this, and neither does anything else that puts
        // a run on a plot. The digest hashes what is here, so the loss is declared, not hidden.
        Ty::U64 => c.u64()? as f64,
        Ty::I64 => c.i64()? as f64,
        Ty::F32 => f64::from(c.f32()?),
        Ty::F64 => c.f64()?,
        Ty::Str => unreachable!("strings are handled by the caller"),
    })
}

fn parse_field(tyname: &str, fname: &str) -> Result<Field, Error> {
    let (base, arity) = match tyname.find('[') {
        None => (tyname, Arity::One),
        Some(i) => {
            let inner = tyname[i + 1..].trim_end_matches(']');
            let a = if inner.is_empty() {
                Arity::Seq
            } else if let Some(bound) = inner.strip_prefix("<=") {
                // A bounded sequence is a sequence on the wire; the bound is a promise, not a
                // layout. Parsed only so a malformed bound is still an error.
                let _: usize = bound.parse().map_err(|_| Error::BadField(tyname.into()))?;
                Arity::Seq
            } else {
                Arity::Fixed(inner.parse().map_err(|_| Error::BadField(tyname.into()))?)
            };
            (&tyname[..i], a)
        }
    };
    // `string<=10` is a bounded string: same wire form, so the bound is dropped.
    let base = base.split("<=").next().unwrap_or(base);
    let kind = if base == "string" {
        Kind::Prim(Ty::Str)
    } else if base == "wstring" {
        return Err(Error::Unsupported("wstring".into()));
    } else if let Some(p) = primitive(base) {
        Kind::Prim(p)
    } else {
        Kind::Msg(base.to_string())
    };
    Ok(Field {
        name: fname.to_string(),
        kind,
        arity,
    })
}
