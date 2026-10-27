//! The `JSON` object: `JSON.stringify` and `JSON.parse`.

use std::borrow::Cow;
use std::rc::Rc;

use crate::builtins::{arg, table_object};
use crate::value::{
    is_index_key, key_order, obj_put_cached, push_number, push_small, to_str, NativeDef,
    Transitions, Value,
};
use crate::vm::Vm;

pub static METHODS: &[NativeDef] = &[
    NativeDef {
        name: "stringify",
        func: stringify,
    },
    NativeDef {
        name: "parse",
        func: parse,
    },
];

/// Nesting depth at which we give up, which also catches cyclic structures.
const MAX_DEPTH: usize = 512;

/// The most property names that a parse keeps shared, so that they are not copied again (see `Parser::shared_key`).
const MAX_SHARED_KEYS: usize = 64;

pub fn json_object() -> Value {
    table_object(METHODS)
}

fn stringify(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let indent = match arg(args, 2) {
        Value::Num(n) => " ".repeat(n.clamp(0.0, 10.0) as usize),
        Value::Str(s) => s.chars().take(10).collect(),
        _ => String::new(),
    };
    let mut out = String::new();
    match write_value(
        &arg(args, 0),
        &indent,
        0,
        &mut out,
        &mut QuotedKeys::default(),
    ) {
        Ok(true) => Ok(Value::from_string(out)),
        Ok(false) => Ok(Value::Undef),
        Err(msg) => Err(vm.make_error("TypeError", msg)),
    }
}

/// The quoted form of the keys that one write has met. Objects of one shape repeat their keys, and a key is shared by
/// its allocation, so a key is quoted once and its text is copied for the objects that come after it.
#[derive(Default)]
struct QuotedKeys {
    entries: Vec<(Rc<str>, String)>,
}

/// The most keys a write keeps quoted. A write that meets more keys quotes the rest each time.
const MAX_QUOTED_KEYS: usize = 32;

impl QuotedKeys {
    /// Appends the quoted form of `key` to `out`.
    fn push(&mut self, key: &Rc<str>, out: &mut String) {
        if let Some((_, quoted)) = self.entries.iter().find(|(k, _)| Rc::ptr_eq(k, key)) {
            push_small(out, quoted);
            return;
        }
        let start = out.len();
        quote(key, out);
        if self.entries.len() < MAX_QUOTED_KEYS {
            self.entries.push((key.clone(), out[start..].to_string()));
        }
    }
}

/// Appends the JSON text for `v`. Returns Ok(false), writing nothing, for values JSON omits.
fn write_value(
    v: &Value,
    indent: &str,
    depth: usize,
    out: &mut String,
    keys: &mut QuotedKeys,
) -> Result<bool, &'static str> {
    if depth > MAX_DEPTH {
        return Err("Converting circular structure to JSON");
    }
    match v {
        Value::Undef | Value::Func(_) | Value::Native(_) | Value::Cell(_) => return Ok(false),
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) if n.is_finite() => push_number(out, *n),
        Value::Num(_) => out.push_str("null"),
        Value::Str(s) => quote(s, out),
        Value::Arr(a) => {
            // Writing never runs user code, so the array is read in place.
            let items = a.borrow();
            if items.is_empty() {
                out.push_str("[]");
                return Ok(true);
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(indent, depth + 1, out);
                if !write_value(item, indent, depth + 1, out, keys)? {
                    out.push_str("null");
                }
            }
            newline(indent, depth, out);
            out.push(']');
        }
        Value::Obj(o) => {
            let props = o.borrow();
            out.push('{');
            let mut written = false;
            // Index keys come first, in ascending order, so an object that has them is ordered by its positions. Any other
            // object is written in the order of its properties, which needs no list of positions.
            if props.iter().any(|(k, _)| is_index_key(k)) {
                for i in key_order(&props) {
                    write_member(
                        props.key(i),
                        props.value(i),
                        indent,
                        depth,
                        out,
                        &mut written,
                        keys,
                    )?;
                }
            } else {
                for (key, val) in props.iter() {
                    write_member(key, val, indent, depth, out, &mut written, keys)?;
                }
            }
            // An object with nothing to show is written as `{}`, with no line break inside.
            if written {
                newline(indent, depth, out);
            }
            out.push('}');
        }
    }
    Ok(true)
}

/// Whether JSON writes `v`. The values it leaves out are omitted from an object, and written as `null` in an array.
fn shows(v: &Value) -> bool {
    !matches!(
        v,
        Value::Undef | Value::Func(_) | Value::Native(_) | Value::Cell(_)
    )
}

/// Writes the property `key: val`, unless JSON leaves its value out. `written` tells whether a property came before it.
fn write_member(
    key: &Rc<str>,
    val: &Value,
    indent: &str,
    depth: usize,
    out: &mut String,
    written: &mut bool,
    keys: &mut QuotedKeys,
) -> Result<(), &'static str> {
    if !shows(val) {
        return Ok(());
    }
    if *written {
        out.push(',');
    }
    *written = true;
    newline(indent, depth + 1, out);
    keys.push(key, out);
    if indent.is_empty() {
        out.push(':');
    } else {
        push_small(out, ": ");
    }
    write_value(val, indent, depth + 1, out, keys)?;
    Ok(())
}

#[inline]
fn newline(indent: &str, depth: usize, out: &mut String) {
    if !indent.is_empty() {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
}

/// The bytes that a JSON string writes escaped: the control characters, the quote and the backslash.
const ESCAPED: [bool; 256] = {
    let mut table = [false; 256];
    let mut b = 0usize;
    while b < 0x20 {
        table[b] = true;
        b += 1;
    }
    table[b'"' as usize] = true;
    table[b'\\' as usize] = true;
    table
};

fn quote(s: &str, out: &mut String) {
    out.push('"');
    // Most strings need no escape, so they are copied whole. Every character that needs one is ASCII, so the bytes
    // are enough to check.
    if !s.bytes().any(|b| ESCAPED[usize::from(b)]) {
        push_small(out, s);
    } else {
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
    }
    out.push('"');
}

fn parse(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let text = to_str(&arg(args, 0));
    let mut p = Parser {
        text: text.as_str(),
        pos: 0,
        keys: Vec::new(),
        transitions: Transitions::default(),
    };
    let result = p.value().and_then(|v| {
        p.skip_ws();
        if p.pos == p.text.len() {
            Ok(v)
        } else {
            Err(p.error("Unexpected token"))
        }
    });
    result.map_err(|msg| vm.make_error("SyntaxError", &msg))
}

/// Reads JSON text. `pos` is a byte offset, and it always falls on a character boundary, since the parser only moves
/// over whole characters.
struct Parser<'a> {
    text: &'a str,
    pos: usize,
    /// The property names read so far, which the objects that have them share (see `shared_key`).
    keys: Vec<Rc<str>>,
    /// The shape transitions of the objects read so far, so that objects of one shape add a key without a lookup.
    transitions: Transitions,
}

/// Whether two texts are the same. A key that a parser reads is short, and the bytes are compared one by one, which
/// costs less than a call into the library's comparison. A long text is compared as a whole.
fn same_text(a: &str, b: &str) -> bool {
    if a.len() > 16 {
        return a == b;
    }
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x == y)
}

impl<'a> Parser<'a> {
    /// The byte at the current position. Every token starts with an ASCII byte, so this is enough to choose a token.
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn rest(&self) -> &'a str {
        let text = self.text;
        &text[self.pos..]
    }

    /// Skips the whitespace at the current position. The common case, which has none, is checked inline.
    #[inline]
    fn skip_ws(&mut self) {
        if matches!(self.byte(), Some(b'\t'..=b'\r' | b' ' | 0x80..=0xff)) {
            self.skip_whitespace_run();
        }
    }

    fn skip_whitespace_run(&mut self) {
        loop {
            match self.byte() {
                // The whitespace `char::is_whitespace` accepts: the ASCII ones here, and some other characters below.
                Some(b'\t'..=b'\r' | b' ') => self.pos += 1,
                Some(b) if !b.is_ascii() => match self.rest().chars().next() {
                    Some(c) if c.is_whitespace() => self.pos += c.len_utf8(),
                    _ => return,
                },
                _ => return,
            }
        }
    }

    /// The error for the token at the current position. The position counts characters, as it always has.
    fn error(&self, what: &str) -> String {
        match self.rest().chars().next() {
            Some(c) => format!(
                "{} '{}' in JSON at position {}",
                what,
                c,
                self.text[..self.pos].chars().count()
            ),
            None => "Unexpected end of JSON input".to_string(),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.byte() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::str(&self.string()?)),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("Unexpected token")),
        }
    }

    fn word(&mut self, text: &str, v: Value) -> Result<Value, String> {
        for &expected in text.as_bytes() {
            if self.byte() != Some(expected) {
                return Err(self.error("Unexpected token"));
            }
            self.pos += 1;
        }
        Ok(v)
    }

    /// A whole number of up to 15 digits, with an optional minus sign, read without the float parser, which gives the same
    /// value. It is taken only when no other character continues the number, and `number` reads the rest.
    fn whole_number(&mut self) -> Option<f64> {
        let bytes = self.text.as_bytes();
        let negative = bytes.get(self.pos) == Some(&b'-');
        let digits = self.pos + usize::from(negative);
        let mut at = digits;
        let mut n: u64 = 0;
        while at - digits < 15 {
            match bytes.get(at) {
                Some(&b @ b'0'..=b'9') => {
                    n = n * 10 + u64::from(b - b'0');
                    at += 1;
                }
                _ => break,
            }
        }
        let continues = matches!(
            bytes.get(at),
            Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        );
        if at == digits || continues {
            return None;
        }
        self.pos = at;
        let v = n as f64;
        Some(if negative { -v } else { v })
    }

    /// The code of an escaped low surrogate that the text continues with, which is consumed, if there is one.
    fn low_surrogate(&mut self) -> Option<u32> {
        let hex = self.rest().strip_prefix("\\u")?.get(..4)?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        if !(0xdc00..0xe000).contains(&code) {
            return None;
        }
        self.pos += 6;
        Some(code)
    }

    /// The shared allocation for the property name `name`. Objects of one shape have the same names, so a name that was
    /// read before is found in `keys` and not copied again. Names past `MAX_SHARED_KEYS` are copied each time.
    fn shared_key(&mut self, name: &str) -> Rc<str> {
        // The length and the last byte are compared before the text, which settles most of the keys that differ.
        let last = name.as_bytes().last();
        if let Some(shared) = self
            .keys
            .iter()
            .find(|k| k.len() == name.len() && k.as_bytes().last() == last && same_text(k, name))
        {
            return shared.clone();
        }
        let shared: Rc<str> = Rc::from(name);
        if self.keys.len() < MAX_SHARED_KEYS {
            self.keys.push(shared.clone());
        }
        shared
    }

    fn number(&mut self) -> Result<Value, String> {
        if let Some(n) = self.whole_number() {
            return Ok(Value::Num(n));
        }
        let start = self.pos;
        while matches!(
            self.byte(),
            Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        ) {
            self.pos += 1;
        }
        self.text[start..self.pos]
            .parse::<f64>()
            .map(Value::Num)
            .map_err(|_| self.error("Unexpected token"))
    }

    /// Reads a string. It is borrowed from the text when it has no escape, so most strings and keys are not copied.
    fn string(&mut self) -> Result<Cow<'a, str>, String> {
        let text = self.text;
        self.pos += 1; // opening quote
                       // A string without an escape is a slice of the text, up to its closing quote, which one scan finds. A string
                       // with an escape, or one that is not closed, takes the loop below from the same point.
        let bytes = text.as_bytes();
        let mut end = self.pos;
        while let Some(&b) = bytes.get(end) {
            if b == b'"' {
                let plain = &text[self.pos..end];
                self.pos = end + 1;
                return Ok(Cow::Borrowed(plain));
            }
            if b == b'\\' {
                break;
            }
            end += 1;
        }
        // The text from `run` on has not been copied into `out` yet. `out` is only made once an escape is read.
        let mut run = self.pos;
        let mut out: Option<String> = None;
        loop {
            let Some(offset) = text.as_bytes()[self.pos..]
                .iter()
                .position(|&b| b == b'"' || b == b'\\')
            else {
                return Err("Unterminated string in JSON".to_string());
            };
            self.pos += offset;
            let plain = &text[run..self.pos];
            if self.byte() == Some(b'"') {
                self.pos += 1;
                return Ok(match out {
                    Some(mut s) => {
                        s.push_str(plain);
                        Cow::Owned(s)
                    }
                    None => Cow::Borrowed(plain),
                });
            }
            let s = out.get_or_insert_with(String::new);
            s.push_str(plain);
            self.pos += 1; // backslash
            let c = self
                .rest()
                .chars()
                .next()
                .ok_or_else(|| "Unterminated string in JSON".to_string())?;
            self.pos += c.len_utf8();
            match c {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                'b' => s.push('\u{8}'),
                'f' => s.push('\u{c}'),
                'u' => {
                    // Four hex digits, or fewer when the text ends first, as the window has always been read.
                    let rest = self.rest();
                    let window = &rest[..rest.char_indices().nth(4).map_or(rest.len(), |(i, _)| i)];
                    let code = u32::from_str_radix(window, 16)
                        .map_err(|_| self.error("Bad unicode escape"))?;
                    self.pos += window.len();
                    // A high surrogate followed by an escaped low surrogate is one character past the Basic Multilingual Plane.
                    let pair = if (0xd800..0xdc00).contains(&code) {
                        self.low_surrogate()
                    } else {
                        None
                    };
                    let character = match pair {
                        Some(low) => {
                            char::from_u32(0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00))
                        }
                        None => char::from_u32(code),
                    };
                    s.push(character.unwrap_or('\u{fffd}'));
                }
                other => s.push(other),
            }
            run = self.pos;
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.byte() == Some(b']') {
            self.pos += 1;
            return Ok(Value::arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.byte() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::arr(items));
                }
                _ => return Err(self.error("Unexpected token")),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let obj = Value::obj();
        self.skip_ws();
        if self.byte() == Some(b'}') {
            self.pos += 1;
            return Ok(obj);
        }
        loop {
            self.skip_ws();
            if self.byte() != Some(b'"') {
                return Err(self.error("Expected property name"));
            }
            let name = self.string()?;
            let key = self.shared_key(&name);
            self.skip_ws();
            if self.byte() != Some(b':') {
                return Err(self.error("Expected ':'"));
            }
            self.pos += 1;
            let v = self.value()?;
            if let Value::Obj(o) = &obj {
                obj_put_cached(o, &key, v, &mut self.transitions);
            }
            self.skip_ws();
            match self.byte() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(obj);
                }
                _ => return Err(self.error("Unexpected token")),
            }
        }
    }
}
