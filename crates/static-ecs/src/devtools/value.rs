//! Component values as data: described through `Debug` where a component
//! implements it (specialization over `'static` types), otherwise through
//! compile-time reflection (`core::mem::type_info`) of its fields. Scalar
//! fields of plain structs can be edited in place, like a debugger writes
//! memory.

use std::any::TypeId;
use std::fmt::{self, Debug, Write as _};
use std::mem::type_info::TypeKind;

/// A component value as a tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i128),
    Float(f64),
    Char(char),
    Str(String),
    /// Named fields: `Pos { x: 1.0, y: 2.0 }`.
    Struct {
        name: String,
        fields: Vec<(String, Value)>,
    },
    /// Positional fields: `Hp(3)`, or a tuple when `name` is empty.
    Tuple {
        name: String,
        items: Vec<Value>,
    },
    List(Vec<Value>),
    Map(Vec<(Value, Value)>),
    /// A bare name: a unit struct or unit variant (`Idle`, `None`).
    Name(String),
    /// Text that is not structured data, or an opaque value.
    Raw(String),
}

impl Value {
    /// The value at `path`: field names and positions separated by `.`
    /// (`pos.x`, `0`, `items.2`, `items[2]`). Empty is the value itself.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut value = self;
        for step in path_steps(path) {
            value = value.child(step)?;
        }
        Some(value)
    }
    fn child(&self, step: &str) -> Option<&Value> {
        match self {
            Value::Struct { fields, .. } => fields.iter().find(|(n, _)| n == step).map(|(_, v)| v),
            Value::Tuple { items, .. } | Value::List(items) => {
                items.get(step.parse::<usize>().ok()?)
            }
            Value::Map(entries) => entries
                .iter()
                .find(|(k, _)| k.to_string() == step || matches!(k, Value::Str(s) if s == step))
                .map(|(_, v)| v),
            _ => None,
        }
    }
    /// As a number, if it is one (booleans count as 0/1).
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Int(i) => Some(i as f64),
            Value::Float(f) => Some(f),
            Value::Bool(b) => Some(b as u8 as f64),
            _ => None,
        }
    }
    /// The text of a string, name, or char.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Str(s) | Value::Name(s) | Value::Raw(s) => Some(s.clone()),
            Value::Char(c) => Some(c.to_string()),
            _ => None,
        }
    }
}

pub(crate) fn path_steps(path: &str) -> impl Iterator<Item = &str> {
    path.split(['.', '[', ']']).filter(|s| !s.is_empty())
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn list(f: &mut fmt::Formatter<'_>, items: &[Value]) -> fmt::Result {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{item}")?;
            }
            Ok(())
        }
        match self {
            Value::Unit => f.write_str("()"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Char(c) => write!(f, "{c:?}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Struct { name, fields } => {
                write!(f, "{name} {{ ")?;
                for (i, (n, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{n}: {v}")?;
                }
                f.write_str(" }")
            }
            Value::Tuple { name, items } => {
                write!(f, "{name}(")?;
                list(f, items)?;
                f.write_str(")")
            }
            Value::List(items) => {
                f.write_str("[")?;
                list(f, items)?;
                f.write_str("]")
            }
            Value::Map(entries) => {
                f.write_str("{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                f.write_str("}")
            }
            Value::Name(n) | Value::Raw(n) => f.write_str(n),
        }
    }
}

/// Parse `Debug` output into a value; anything unparsable stays `Raw`.
pub fn parse_debug(text: &str) -> Value {
    let mut parser = Parser {
        text: text.as_bytes(),
        at: 0,
    };
    match parser.value() {
        Some(value) if parser.done() => value,
        _ => Value::Raw(text.to_string()),
    }
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn skip(&mut self) {
        while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }
    fn done(&mut self) -> bool {
        self.skip();
        self.at == self.text.len()
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.text.get(self.at).copied()
    }
    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<String> {
        self.skip();
        let start = self.at;
        while self.at < self.text.len() {
            let b = self.text[self.at];
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.at += 1;
            } else if b == b':' && self.text.get(self.at + 1) == Some(&b':') {
                self.at += 2;
            } else {
                break;
            }
        }
        (self.at > start).then(|| String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }
    fn items(&mut self, close: u8) -> Option<Vec<Value>> {
        let mut items = Vec::new();
        loop {
            if self.eat(close) {
                return Some(items);
            }
            items.push(self.value()?);
            if !self.eat(b',') {
                return self.eat(close).then_some(items);
            }
        }
    }
    fn value(&mut self) -> Option<Value> {
        match self.peek()? {
            b'"' => self.string().map(Value::Str),
            b'\'' => self.char_literal(),
            b'(' => {
                self.at += 1;
                let items = self.items(b')')?;
                Some(if items.is_empty() {
                    Value::Unit
                } else {
                    Value::Tuple {
                        name: String::new(),
                        items,
                    }
                })
            }
            b'[' => {
                self.at += 1;
                self.items(b']').map(Value::List)
            }
            b'{' => {
                self.at += 1;
                self.map()
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => {
                let name = self.name()?;
                match name.as_str() {
                    "true" => return Some(Value::Bool(true)),
                    "false" => return Some(Value::Bool(false)),
                    "NaN" => return Some(Value::Float(f64::NAN)),
                    "inf" => return Some(Value::Float(f64::INFINITY)),
                    _ => {}
                }
                if self.eat(b'(') {
                    let items = self.items(b')')?;
                    Some(Value::Tuple { name, items })
                } else if self.eat(b'{') {
                    let mut fields = Vec::new();
                    loop {
                        if self.eat(b'}') {
                            break;
                        }
                        if self.peek() == Some(b'.') {
                            // `..` of a `finish_non_exhaustive`.
                            self.at += 2;
                            continue;
                        }
                        let field = self.name()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        fields.push((field, self.value()?));
                        if !self.eat(b',') && !self.eat(b'}') {
                            return None;
                        } else if self.text[self.at - 1] == b'}' {
                            break;
                        }
                    }
                    Some(Value::Struct { name, fields })
                } else {
                    Some(Value::Name(name))
                }
            }
        }
    }
    fn map(&mut self) -> Option<Value> {
        let mut entries = Vec::new();
        let mut set = Vec::new();
        loop {
            if self.eat(b'}') {
                break;
            }
            let key = self.value()?;
            if self.eat(b':') {
                entries.push((key, self.value()?));
            } else {
                set.push(key);
            }
            if !self.eat(b',') {
                if !self.eat(b'}') {
                    return None;
                }
                break;
            }
        }
        Some(if set.is_empty() {
            Value::Map(entries)
        } else {
            Value::List(set)
        })
    }
    fn number(&mut self) -> Option<Value> {
        self.skip();
        let start = self.at;
        if self.text[self.at] == b'-' {
            self.at += 1;
        }
        while self.at < self.text.len()
            && (self.text[self.at].is_ascii_alphanumeric()
                || matches!(self.text[self.at], b'.' | b'_'))
            || (self.at > start
                && matches!(self.text[self.at - 1], b'e' | b'E')
                && self.text.get(self.at) == Some(&b'-'))
        {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        if text == "-inf" {
            return Some(Value::Float(f64::NEG_INFINITY));
        }
        if let Ok(i) = text.parse::<i128>() {
            Some(Value::Int(i))
        } else {
            text.parse::<f64>().ok().map(Value::Float)
        }
    }
    fn string(&mut self) -> Option<String> {
        self.at += 1;
        let mut out = String::new();
        let mut bytes = Vec::new();
        while self.at < self.text.len() {
            let b = self.text[self.at];
            self.at += 1;
            match b {
                b'"' => {
                    out.push_str(&String::from_utf8_lossy(&bytes));
                    return Some(out);
                }
                b'\\' => {
                    let e = *self.text.get(self.at)?;
                    self.at += 1;
                    let c = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'0' => '\0',
                        b'u' => {
                            // \u{XXXX}
                            let close = self.text[self.at..].iter().position(|&b| b == b'}')?;
                            let hex = std::str::from_utf8(&self.text[self.at + 1..self.at + close])
                                .ok()?;
                            self.at += close + 1;
                            char::from_u32(u32::from_str_radix(hex, 16).ok()?)?
                        }
                        other => other as char,
                    };
                    let mut buf = [0; 4];
                    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                _ => bytes.push(b),
            }
        }
        None
    }
    fn char_literal(&mut self) -> Option<Value> {
        let start = self.at;
        self.at += 1;
        while self.at < self.text.len() && self.text[self.at] != b'\'' {
            if self.text[self.at] == b'\\' {
                self.at += 1;
            }
            self.at += 1;
        }
        self.at += 1;
        let inner = std::str::from_utf8(self.text.get(start + 1..self.at - 1)?).ok()?;
        let mut chars = inner.chars();
        let c = match (chars.next()?, chars.next()) {
            ('\\', Some('n')) => '\n',
            ('\\', Some('t')) => '\t',
            ('\\', Some(c)) => c,
            (c, None) => c,
            _ => return None,
        };
        Some(Value::Char(c))
    }
}

/// How a reflected field is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int {
        signed: bool,
    },
    Float,
    Bool,
    Char,
    /// A struct whose fields follow.
    Struct,
    /// Not described field by field (pointer, enum, tuple, array, ...).
    Opaque,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Field {
    pub name: &'static str,
    /// Index of the containing field; `u16::MAX` for the root.
    pub parent: u16,
    pub offset: usize,
    pub size: usize,
    pub kind: Kind,
    /// Every field below is a scalar or a plain struct: writable in place.
    pub plain: bool,
}
const NONE: Field = Field {
    name: "",
    parent: 0,
    offset: 0,
    size: 0,
    kind: Kind::Opaque,
    plain: false,
};
const MAX_FIELDS: usize = 64;
const MAX_DEPTH: usize = 6;
/// A type's fields from compile-time reflection, depth first from the root.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub fields: [Field; MAX_FIELDS],
    pub len: usize,
    /// More fields than recorded.
    pub truncated: bool,
}

/// Types whose fields hold validity invariants: never written field-wise.
const fn guarded(t: TypeId) -> bool {
    use std::num::NonZero;
    let guarded = [
        TypeId::of::<NonZero<u8>>(),
        TypeId::of::<NonZero<u16>>(),
        TypeId::of::<NonZero<u32>>(),
        TypeId::of::<NonZero<u64>>(),
        TypeId::of::<NonZero<u128>>(),
        TypeId::of::<NonZero<usize>>(),
        TypeId::of::<NonZero<i8>>(),
        TypeId::of::<NonZero<i16>>(),
        TypeId::of::<NonZero<i32>>(),
        TypeId::of::<NonZero<i64>>(),
        TypeId::of::<NonZero<i128>>(),
        TypeId::of::<NonZero<isize>>(),
    ];
    let mut i = 0;
    while i < guarded.len() {
        if guarded[i] == t {
            return true;
        }
        i += 1;
    }
    false
}

/// Compile-time field layouts.
pub(crate) trait Reflect {
    const LAYOUT: Layout;
}
/// A type name with every path shortened to its last segment:
/// `Query<'_, &Hp, With<Dead>>`.
pub fn short_type(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut word = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            word.push(c);
        } else {
            out.push_str(word.rsplit("::").next().unwrap_or(&word));
            word.clear();
            out.push(c);
        }
    }
    out.push_str(word.rsplit("::").next().unwrap_or(&word));
    out
}
impl<T: 'static> Reflect for T {
    const LAYOUT: Layout = {
        let mut out = Layout {
            fields: [NONE; MAX_FIELDS],
            len: 0,
            truncated: false,
        };
        // (type, parent, offset, name, depth), depth first via an explicit stack.
        let mut stack: [(TypeId, u16, usize, &'static str, usize); MAX_FIELDS] =
            [(TypeId::of::<()>(), 0, 0, "", 0); MAX_FIELDS];
        stack[0] = (TypeId::of::<T>(), u16::MAX, 0, "", 0);
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let (t, parent, offset, name, depth) = stack[sp];
            if out.len == MAX_FIELDS {
                out.truncated = true;
                break;
            }
            let size = match t.size() {
                Some(size) => size,
                None => 0,
            };
            let kind = if guarded(t) {
                Kind::Opaque
            } else {
                match t.info().kind {
                    TypeKind::Int => Kind::Int {
                        signed: t.is_signed(),
                    },
                    TypeKind::Float => Kind::Float,
                    TypeKind::Bool => Kind::Bool,
                    TypeKind::Char => Kind::Char,
                    // Tuples' fields are not reflected yet; enums need their
                    // active variant, which only `Debug` reveals.
                    TypeKind::Struct if depth < MAX_DEPTH => Kind::Struct,
                    _ => Kind::Opaque,
                }
            };
            let index = out.len;
            out.fields[index] = Field {
                name,
                parent,
                offset,
                size,
                kind,
                plain: !matches!(kind, Kind::Opaque),
            };
            out.len += 1;
            if matches!(kind, Kind::Struct) {
                let count = t.fields(0);
                if sp + count > MAX_FIELDS {
                    out.truncated = true;
                    out.fields[index].kind = Kind::Opaque;
                    out.fields[index].plain = false;
                } else {
                    // Pushed in reverse so they pop in declaration order.
                    let mut i = count;
                    while i > 0 {
                        i -= 1;
                        let field = t.field(0, i);
                        stack[sp] = (
                            field.type_id(),
                            index as u16,
                            offset + field.offset(),
                            field.name(),
                            depth + 1,
                        );
                        sp += 1;
                    }
                }
            }
        }
        // A struct is plain when every field below it is; children follow
        // their parents, so one backward pass settles every struct.
        let mut i = out.len;
        while i > 1 {
            i -= 1;
            let parent = out.fields[i].parent as usize;
            if !out.fields[i].plain {
                out.fields[parent].plain = false;
            }
        }
        out
    };
}

/// Read a reflected field of the value at `base`.
/// # Safety
/// `base` points at a live value whose layout is `layout`.
unsafe fn read_field(layout: &Layout, index: usize, base: *const u8, type_name: &str) -> Value {
    let field = layout.fields[index];
    let at = unsafe { base.add(field.offset) };
    unsafe {
        match (field.kind, field.size) {
            (Kind::Int { signed: true }, 1) => Value::Int(at.cast::<i8>().read_unaligned() as i128),
            (Kind::Int { signed: true }, 2) => {
                Value::Int(at.cast::<i16>().read_unaligned() as i128)
            }
            (Kind::Int { signed: true }, 4) => {
                Value::Int(at.cast::<i32>().read_unaligned() as i128)
            }
            (Kind::Int { signed: true }, 8) => {
                Value::Int(at.cast::<i64>().read_unaligned() as i128)
            }
            (Kind::Int { signed: true }, 16) => Value::Int(at.cast::<i128>().read_unaligned()),
            (Kind::Int { signed: false }, 1) => Value::Int(at.read() as i128),
            (Kind::Int { signed: false }, 2) => {
                Value::Int(at.cast::<u16>().read_unaligned() as i128)
            }
            (Kind::Int { signed: false }, 4) => {
                Value::Int(at.cast::<u32>().read_unaligned() as i128)
            }
            (Kind::Int { signed: false }, 8) => {
                Value::Int(at.cast::<u64>().read_unaligned() as i128)
            }
            (Kind::Int { signed: false }, 16) => {
                Value::Int(at.cast::<u128>().read_unaligned().min(i128::MAX as u128) as i128)
            }
            (Kind::Float, 4) => Value::Float(at.cast::<f32>().read_unaligned() as f64),
            (Kind::Float, 8) => Value::Float(at.cast::<f64>().read_unaligned()),
            (Kind::Bool, _) => Value::Bool(at.read() != 0),
            (Kind::Char, _) => {
                Value::Char(char::from_u32(at.cast::<u32>().read_unaligned()).unwrap_or('\u{fffd}'))
            }
            (Kind::Struct, _) => {
                let fields = (index + 1..layout.len)
                    .filter(|&child| layout.fields[child].parent as usize == index)
                    .map(|child| {
                        (
                            layout.fields[child].name.to_string(),
                            read_field(layout, child, base, "?"),
                        )
                    })
                    .collect();
                Value::Struct {
                    name: short_name(type_name).to_string(),
                    fields,
                }
            }
            _ => Value::Raw(format!("<opaque, {} bytes>", field.size)),
        }
    }
}

/// The last path segment of a type name, generics kept.
pub fn short_name(name: &str) -> &str {
    let end = name.find('<').unwrap_or(name.len());
    let start = name[..end].rfind("::").map_or(0, |i| i + 2);
    &name[start..]
}

/// Describes a component type's values.
pub(crate) trait Describe {
    /// # Safety
    /// `value` points at a live `Self`.
    unsafe fn describe(value: *const u8) -> Value;
}
impl<T: 'static> Describe for T {
    default unsafe fn describe(value: *const u8) -> Value {
        let layout = &<T as Reflect>::LAYOUT;
        if layout.len > 0 && layout.fields[0].kind != Kind::Opaque {
            unsafe { read_field(layout, 0, value, std::any::type_name::<T>()) }
        } else {
            Value::Raw(format!(
                "<{} without Debug, {} bytes>",
                short_name(std::any::type_name::<T>()),
                size_of::<T>()
            ))
        }
    }
}
impl<T: Debug + 'static> Describe for T {
    unsafe fn describe(value: *const u8) -> Value {
        let mut text = String::new();
        let _ = write!(text, "{:?}", unsafe { &*value.cast::<T>() });
        parse_debug(&text)
    }
}

/// Write `literal` into the scalar field at `path` of the value at `base`.
/// # Safety
/// `base` points at a live, exclusively borrowed `T`.
pub(crate) unsafe fn edit<T: 'static>(
    base: *mut u8,
    path: &str,
    literal: &str,
) -> Result<Value, String> {
    let layout = &<T as Reflect>::LAYOUT;
    let mut index = 0;
    if layout.len == 0 || layout.fields[0].kind == Kind::Opaque {
        return Err(format!(
            "`{}` has no reflected fields to edit",
            std::any::type_name::<T>()
        ));
    }
    for step in path_steps(path) {
        let found = (index + 1..layout.len).find(|&child| {
            let field = &layout.fields[child];
            field.parent as usize == index && field.name == step
        });
        index = found.ok_or_else(|| format!("no field `{step}` in `{path}`"))?;
    }
    let field = layout.fields[index];
    if !field.plain || field.kind == Kind::Struct {
        return Err(format!(
            "`{path}` is not a scalar inside plain structs (pointers, enums, and private std internals are never written)"
        ));
    }
    let at = unsafe { base.add(field.offset) };
    let literal = literal.trim();
    let bad = || {
        format!(
            "`{literal}` is not a valid {:?} of {} bytes",
            field.kind, field.size
        )
    };
    unsafe {
        match field.kind {
            Kind::Int { signed } => {
                let v: i128 = literal.replace('_', "").parse().map_err(|_| bad())?;
                let bits = field.size * 8;
                let fits = if signed {
                    bits == 128 || (v >= -(1i128 << (bits - 1)) && v < (1i128 << (bits - 1)))
                } else {
                    v >= 0 && (bits == 128 || v < (1i128 << bits))
                };
                if !fits {
                    return Err(bad());
                }
                let bytes = v.to_le_bytes();
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, field.size);
            }
            Kind::Float => {
                let v: f64 = literal.parse().map_err(|_| bad())?;
                match field.size {
                    4 => at.cast::<f32>().write_unaligned(v as f32),
                    8 => at.cast::<f64>().write_unaligned(v),
                    _ => return Err(bad()),
                }
            }
            Kind::Bool => at.write(match literal {
                "true" => 1,
                "false" => 0,
                _ => return Err(bad()),
            }),
            Kind::Char => {
                let mut chars = literal.trim_matches('\'').chars();
                let c = chars
                    .next()
                    .filter(|_| chars.next().is_none())
                    .ok_or_else(bad)?;
                at.cast::<u32>().write_unaligned(c as u32);
            }
            Kind::Struct | Kind::Opaque => unreachable!(),
        }
        Ok(read_field(layout, index, base, "?"))
    }
}

/// The field paths of `T` that `edit` accepts.
pub(crate) fn editable<T: 'static>() -> Vec<String> {
    let layout = &<T as Reflect>::LAYOUT;
    let path = |mut index: usize| {
        let mut parts = Vec::new();
        while index != 0 {
            parts.push(layout.fields[index].name);
            index = layout.fields[index].parent as usize;
        }
        parts.reverse();
        parts.join(".")
    };
    (1..layout.len)
        .filter(|&i| layout.fields[i].plain && layout.fields[i].kind != Kind::Struct)
        .map(path)
        .collect()
}
