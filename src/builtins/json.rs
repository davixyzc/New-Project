//! The `JSON` object: `JSON.stringify` and `JSON.parse`.

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
            let shown: Vec<usize> = key_order(&props)
                .into_iter()
                .filter(|&i| shows(props.value(i)))
                .collect();
            if shown.is_empty() {
                out.push_str("{}");
                return Ok(true);
            }
            let sep = if indent.is_empty() { ":" } else { ": " };
            out.push('{');
            for (n, &i) in shown.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                let (k, val) = (props.key(i), props.value(i));
                newline(indent, depth + 1, out);
                quote(k, out);
                out.push_str(sep);
                write_value(val, indent, depth + 1, out)?;
            }
            newline(indent, depth, out);
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
    // Most strings need no escape, so they are copied whole.
    if !s.chars().any(|c| c < ' ' || c == '"' || c == '\\') {
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
        chars: text.chars().collect(),
        pos: 0,
    };
    let result = p.value().and_then(|v| {
        p.skip_ws();
        if p.pos == p.chars.len() {
            Ok(v)
        } else {
            Err(p.error("Unexpected token"))
        }
    });
    result.map_err(|msg| vm.make_error("SyntaxError", &msg))
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn error(&self, what: &str) -> String {
        match self.peek() {
            Some(c) => format!("{} '{}' in JSON at position {}", what, c, self.pos),
            None => "Unexpected end of JSON input".to_string(),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Value::from_string(self.string()?)),
            Some('t') => self.word("true", Value::Bool(true)),
            Some('f') => self.word("false", Value::Bool(false)),
            Some('n') => self.word("null", Value::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.error("Unexpected token")),
        }
    }

    fn word(&mut self, text: &str, v: Value) -> Result<Value, String> {
        for expected in text.chars() {
            if self.peek() != Some(expected) {
                return Err(self.error("Unexpected token"));
            }
            self.pos += 1;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Value::Num)
            .map_err(|_| self.error("Unexpected token"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("Unterminated string in JSON".to_string()),
                Some('"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    let c = self
                        .peek()
                        .ok_or_else(|| "Unterminated string in JSON".to_string())?;
                    self.pos += 1;
                    match c {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let hex: String = self.chars.iter().skip(self.pos).take(4).collect();
                            let code = u32::from_str_radix(&hex, 16)
                                .map_err(|_| self.error("Bad unicode escape"))?;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            self.pos += 4;
                        }
                        other => out.push(other),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
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
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(obj);
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(self.error("Expected property name"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(self.error("Expected ':'"));
            }
            self.pos += 1;
            let v = self.value()?;
            if let Value::Obj(o) = &obj {
                obj_set(o, &key, v);
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(obj);
                }
                _ => return Err(self.error("Unexpected token")),
            }
        }
    }
}
