//! The `JSON` object: `JSON.stringify` and `JSON.parse`.

use std::borrow::Cow;

use crate::builtins::{arg, table_object};
use crate::value::{key_order, obj_set, push_number, to_str, NativeDef, Value};
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
    match write_value(&arg(args, 0), &indent, 0, &mut out) {
        Ok(true) => Ok(Value::from_string(out)),
        Ok(false) => Ok(Value::Undef),
        Err(msg) => Err(vm.make_error("TypeError", msg)),
    }
}

/// Appends the JSON text for `v`. Returns Ok(false), writing nothing, for values JSON omits.
fn write_value(
    v: &Value,
    indent: &str,
    depth: usize,
    out: &mut String,
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
                if !write_value(item, indent, depth + 1, out)? {
                    out.push_str("null");
                }
            }
            newline(indent, depth, out);
            out.push(']');
        }
        Value::Obj(o) => {
            let props = o.borrow();
            let sep = if indent.is_empty() { ":" } else { ": " };
            out.push('{');
            let mut written = false;
            for i in key_order(&props) {
                let val = props.value(i);
                if !shows(val) {
                    continue;
                }
                if written {
                    out.push(',');
                }
                written = true;
                newline(indent, depth + 1, out);
                quote(props.key(i), out);
                out.push_str(sep);
                write_value(val, indent, depth + 1, out)?;
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

fn newline(indent: &str, depth: usize, out: &mut String) {
    if !indent.is_empty() {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    // Most strings need no escape, so they are copied whole. Every character that needs one is ASCII, so the bytes
    // are enough to check.
    if !s.bytes().any(|b| b < b' ' || b == b'"' || b == b'\\') {
        out.push_str(s);
    } else {
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
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

    fn skip_ws(&mut self) {
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

    fn number(&mut self) -> Result<Value, String> {
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
                    s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    self.pos += window.len();
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
            let key = self.string()?;
            self.skip_ws();
            if self.byte() != Some(b':') {
                return Err(self.error("Expected ':'"));
            }
            self.pos += 1;
            let v = self.value()?;
            if let Value::Obj(o) = &obj {
                obj_set(o, &key, v);
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
