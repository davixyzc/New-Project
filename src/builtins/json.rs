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
        &mut QuotedKeys::new(&indent),
    ) {
        Ok(true) => Ok(Value::from_string(out)),
        Ok(false) => Ok(Value::Undef),
        Err(msg) => Err(vm.make_error("TypeError", msg)),
    }
}

/// The quoted form of the keys that one write has met. Objects of one shape repeat their keys, and a key is shared by
/// its allocation, so a key is quoted once and its text is copied for the objects that come after it.
struct QuotedKeys {
    /// The quoted key of each name, followed by the separator that comes after it, which is the same for the whole write.
    entries: Vec<(Rc<str>, String)>,
    /// The entry that the last key matched. The keys of the next object come in the same order, so the entry after it is
    /// the one that a key is looked for first.
    last: usize,
    /// What comes between a key and its value: a colon, or a colon and a space when the output is indented.
    sep: &'static str,
}

/// The most keys a write keeps quoted. A write that meets more keys quotes the rest each time.
const MAX_QUOTED_KEYS: usize = 32;

impl QuotedKeys {
    fn new(indent: &str) -> QuotedKeys {
        QuotedKeys {
            entries: Vec::new(),
            last: 0,
            sep: if indent.is_empty() { ":" } else { ": " },
        }
    }

    /// Appends the quoted form of `key`, and the separator after it, to `out`.
    fn push(&mut self, key: &Rc<str>, out: &mut String) {
        let guess = if self.last + 1 < self.entries.len() {
            self.last + 1
        } else {
            0
        };
        if let Some((k, quoted)) = self.entries.get(guess) {
            if Rc::ptr_eq(k, key) {
                self.last = guess;
                push_small(out, quoted);
                return;
            }
        }
        if let Some(at) = self.entries.iter().position(|(k, _)| Rc::ptr_eq(k, key)) {
            self.last = at;
            push_small(out, &self.entries[at].1);
            return;
        }
        let start = out.len();
        quote(key, out);
        push_small(out, self.sep);
        if self.entries.len() < MAX_QUOTED_KEYS {
            self.last = self.entries.len();
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
            if props.iter().any(|(k, _)| {
                k.as_bytes().first().is_some_and(u8::is_ascii_digit) && is_index_key(k)
            }) {
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

/// Whether some byte of `bytes` is one that a JSON string writes escaped (see `ESCAPED`). Eight bytes are tested at a time
/// with bit tricks, which answer only whether some byte of the word has the property, not which one. A true answer leads
/// the caller to the byte-by-byte copy, which then escapes exactly the bytes that need it.
fn needs_escape(bytes: &[u8]) -> bool {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;
    // A byte is below 0x20, or equal to the quote, or equal to the backslash.
    let has_zero = |v: u64| v.wrapping_sub(ONES) & !v & HIGHS;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let w = u64::from_le_bytes(word);
        let below = w.wrapping_sub(ONES * 0x20) & !w & HIGHS;
        if below | has_zero(w ^ (ONES * u64::from(b'"'))) | has_zero(w ^ (ONES * u64::from(b'\\')))
            != 0
        {
            return true;
        }
    }
    chunks.remainder().iter().any(|&b| ESCAPED[usize::from(b)])
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    // Most strings need no escape, so they are copied whole. Every character that needs one is ASCII, so the bytes
    // are enough to check.
    if !needs_escape(s.as_bytes()) {
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
    /// The byte at the current position, or zero past the end. No token starts with a zero byte, and the error for a token
    /// is made from the text (see `error`), so both read the same way.
    #[inline(always)]
    fn peek(&self) -> u8 {
        self.text.as_bytes().get(self.pos).copied().unwrap_or(0)
    }

    fn rest(&self) -> &'a str {
        let text = self.text;
        &text[self.pos..]
    }

    /// Skips the whitespace at the current position: the ASCII whitespace, and the other characters that `char::is_whitespace`
    /// accepts. The ASCII case is inline, since most tokens are next to none or to a few spaces.
    #[inline(always)]
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                b'\t'..=b'\r' | b' ' => self.pos += 1,
                0x80.. => {
                    if !self.skip_unicode_space() {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    /// Skips one character at the current position if it is a non-ASCII whitespace character, and tells whether it did.
    #[inline(never)]
    fn skip_unicode_space(&mut self) -> bool {
        match self.rest().chars().next() {
            Some(c) if c.is_whitespace() => {
                self.pos += c.len_utf8();
                true
            }
            _ => false,
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
        match self.peek() {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => {
                let text = self.string_text()?;
                Ok(Value::str(&text))
            }
            b't' => self.word("true", Value::Bool(true)),
            b'f' => self.word("false", Value::Bool(false)),
            b'n' => self.word("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.error("Unexpected token")),
        }
    }

    fn word(&mut self, text: &str, v: Value) -> Result<Value, String> {
        for &expected in text.as_bytes() {
            if self.peek() != expected {
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
        while matches!(self.peek(), b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
            self.pos += 1;
        }
        self.text[start..self.pos]
            .parse::<f64>()
            .map(Value::Num)
            .map_err(|_| self.error("Unexpected token"))
    }

    /// The text of the string at the current position, when it has no escape and is closed, which the position moves past.
    /// Otherwise the position stays where it is, and `escaped_string` reads the string.
    #[inline(always)]
    fn plain_string(&mut self) -> Option<&'a str> {
        let bytes = self.text.as_bytes();
        let start = self.pos + 1;
        let mut end = start;
        while end < bytes.len() {
            match bytes[end] {
                b'"' => {
                    self.pos = end + 1;
                    // SAFETY: the slice starts after an ASCII quote and ends at one, which are character boundaries of the
                    // text, so the slice is valid UTF-8 like the text.
                    return Some(unsafe { std::str::from_utf8_unchecked(&bytes[start..end]) });
                }
                b'\\' => return None,
                _ => end += 1,
            }
        }
        None
    }

    /// Reads the string at the current position. It is borrowed from the text when it has no escape, so most strings and
    /// keys are not copied.
    #[inline(always)]
    fn string_text(&mut self) -> Result<Cow<'a, str>, String> {
        match self.plain_string() {
            Some(text) => Ok(Cow::Borrowed(text)),
            None => self.escaped_string(),
        }
    }

    /// Reads a string that has an escape, or that is not closed, from the position of its opening quote.
    #[inline(never)]
    fn escaped_string(&mut self) -> Result<Cow<'a, str>, String> {
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
            if self.peek() == b'"' {
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
        if self.peek() == b']' {
            self.pos += 1;
            return Ok(Value::arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                b',' => self.pos += 1,
                b']' => {
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
        if self.peek() == b'}' {
            self.pos += 1;
            return Ok(obj);
        }
        loop {
            self.skip_ws();
            if self.peek() != b'"' {
                return Err(self.error("Expected property name"));
            }
            let name = self.string_text()?;
            let key = self.shared_key(&name);
            self.skip_ws();
            if self.peek() != b':' {
                return Err(self.error("Expected ':'"));
            }
            self.pos += 1;
            let v = self.value()?;
            if let Value::Obj(o) = &obj {
                obj_put_cached(o, &key, v, &mut self.transitions);
            }
            self.skip_ws();
            match self.peek() {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Ok(obj);
                }
                _ => return Err(self.error("Unexpected token")),
            }
        }
    }
}
