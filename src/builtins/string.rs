//! String and number methods. Strings are UTF-8; indices count characters, not bytes.

use crate::builtins::arg;
use crate::builtins::array::bounds;
use crate::value::{num_to_string, to_num, to_str, NativeDef, Value, MAX_STRING_LEN};
use crate::vm::Vm;

pub static METHODS: &[NativeDef] = &[
    NativeDef {
        name: "charAt",
        func: char_at_method,
    },
    NativeDef {
        name: "charCodeAt",
        func: char_code_at,
    },
    NativeDef {
        name: "indexOf",
        func: index_of,
    },
    NativeDef {
        name: "lastIndexOf",
        func: last_index_of,
    },
    NativeDef {
        name: "includes",
        func: includes,
    },
    NativeDef {
        name: "startsWith",
        func: starts_with,
    },
    NativeDef {
        name: "endsWith",
        func: ends_with,
    },
    NativeDef {
        name: "slice",
        func: slice,
    },
    NativeDef {
        name: "substring",
        func: substring,
    },
    NativeDef {
        name: "toUpperCase",
        func: to_upper,
    },
    NativeDef {
        name: "toLowerCase",
        func: to_lower,
    },
    NativeDef {
        name: "trim",
        func: trim,
    },
    NativeDef {
        name: "trimStart",
        func: trim_start,
    },
    NativeDef {
        name: "trimEnd",
        func: trim_end,
    },
    NativeDef {
        name: "split",
        func: split,
    },
    NativeDef {
        name: "repeat",
        func: repeat,
    },
    NativeDef {
        name: "replace",
        func: replace,
    },
    NativeDef {
        name: "padStart",
        func: pad_start,
    },
    NativeDef {
        name: "padEnd",
        func: pad_end,
    },
    NativeDef {
        name: "concat",
        func: concat,
    },
    NativeDef {
        name: "toString",
        func: to_string,
    },
];

pub static NUMBER_METHODS: &[NativeDef] = &[
    NativeDef {
        name: "toFixed",
        func: to_fixed,
    },
    NativeDef {
        name: "toString",
        func: number_to_string,
    },
];

pub fn method(name: &str) -> Option<&'static NativeDef> {
    METHODS.iter().find(|d| d.name == name)
}

pub fn number_method(name: &str) -> Option<&'static NativeDef> {
    NUMBER_METHODS.iter().find(|d| d.name == name)
}

/// Length in characters.
pub fn char_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

/// The character at a character index.
pub fn char_at(s: &str, i: usize) -> Option<char> {
    if s.is_ascii() {
        s.as_bytes().get(i).map(|b| *b as char)
    } else {
        s.chars().nth(i)
    }
}

fn this_str(this: &Value) -> String {
    match this {
        Value::Str(s) => s.to_string(),
        other => to_str(other),
    }
}

fn str_arg(args: &[Value], i: usize) -> String {
    to_str(&arg(args, i))
}

/// A character index argument: NaN counts as 0 and a fraction is cut off. A negative index names no character.
fn index_arg(v: &Value) -> Option<usize> {
    let n = to_num(v);
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    (n >= 0.0).then(|| n as usize)
}

/// A character position argument, cut to a whole number and kept within `0..=len`. A missing argument gives
/// `default`, and NaN gives 0.
fn position_arg(args: &[Value], i: usize, default: usize, len: usize) -> usize {
    match args.get(i) {
        None | Some(Value::Undef) => default,
        Some(v) => {
            let n = to_num(v);
            if n.is_nan() {
                0
            } else {
                n.trunc().clamp(0.0, len as f64) as usize
            }
        }
    }
}

/// The byte offset of character `pos`, which is at most the length of `s` in characters.
fn byte_offset(s: &str, pos: usize) -> usize {
    s.char_indices().nth(pos).map_or(s.len(), |(b, _)| b)
}

fn last_index_of(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let needle = str_arg(args, 0);
    let len = s.chars().count();
    let m = needle.chars().count();
    if m > len {
        return Ok(Value::Num(-1.0));
    }
    // A missing or NaN position lets the match start anywhere, as in JavaScript.
    let from = match arg(args, 1) {
        Value::Undef => len,
        v => {
            let n = to_num(&v);
            if n.is_nan() {
                len
            } else {
                n.trunc().clamp(0.0, len as f64) as usize
            }
        }
    };
    let offsets: Vec<usize> = s
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(s.len()))
        .collect();
    let found = (0..=from.min(len - m))
        .rev()
        .find(|&k| s[offsets[k]..].starts_with(needle.as_str()));
    Ok(Value::Num(found.map_or(-1.0, |k| k as f64)))
}

fn char_at_method(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let i = index_arg(&arg(args, 0));
    Ok(Value::str(
        &i.and_then(|i| char_at(&s, i))
            .map(String::from)
            .unwrap_or_default(),
    ))
}

fn char_code_at(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let i = index_arg(&arg(args, 0));
    Ok(Value::Num(
        i.and_then(|i| char_at(&s, i))
            .map_or(f64::NAN, |c| c as u32 as f64),
    ))
}

fn index_of(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let from = position_arg(args, 1, 0, s.chars().count());
    let start = byte_offset(&s, from);
    let found = match s[start..].find(&str_arg(args, 0)) {
        Some(byte) => s[..start + byte].chars().count() as f64,
        None => -1.0,
    };
    Ok(Value::Num(found))
}

fn includes(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let start = byte_offset(&s, position_arg(args, 1, 0, s.chars().count()));
    Ok(Value::Bool(s[start..].contains(&str_arg(args, 0))))
}

fn starts_with(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let start = byte_offset(&s, position_arg(args, 1, 0, s.chars().count()));
    Ok(Value::Bool(s[start..].starts_with(&str_arg(args, 0))))
}

fn ends_with(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let len = s.chars().count();
    let end = byte_offset(&s, position_arg(args, 1, len, len));
    Ok(Value::Bool(s[..end].ends_with(&str_arg(args, 0))))
}

fn slice(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let chars: Vec<char> = this_str(&this).chars().collect();
    let (s, e) = bounds(chars.len(), &arg(args, 0), &arg(args, 1));
    Ok(Value::str(&chars[s..e].iter().collect::<String>()))
}

fn substring(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let chars: Vec<char> = this_str(&this).chars().collect();
    let len = chars.len() as f64;
    let clamp = |v: f64| if v.is_nan() { 0.0 } else { v.clamp(0.0, len) } as usize;
    let a = clamp(to_num(&arg(args, 0)));
    let b = match arg(args, 1) {
        Value::Undef => chars.len(),
        v => clamp(to_num(&v)),
    };
    let (s, e) = if a <= b { (a, b) } else { (b, a) };
    Ok(Value::str(&chars[s..e].iter().collect::<String>()))
}

fn to_upper(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&this_str(&this).to_uppercase()))
}

fn to_lower(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&this_str(&this).to_lowercase()))
}

fn trim(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(this_str(&this).trim()))
}

fn trim_start(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(this_str(&this).trim_start()))
}

fn trim_end(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(this_str(&this).trim_end()))
}

fn split(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let mut parts: Vec<Value> = match arg(args, 0) {
        Value::Undef => vec![Value::str(&s)],
        sep => {
            let sep = to_str(&sep);
            if sep.is_empty() {
                s.chars().map(|c| Value::str(&c.to_string())).collect()
            } else {
                s.split(sep.as_str()).map(Value::str).collect()
            }
        }
    };
    // The optional limit keeps that many pieces. It is read as ToUint32 reads it.
    if !matches!(arg(args, 1), Value::Undef) {
        let limit = to_num(&arg(args, 1));
        let limit = if limit.is_finite() {
            limit.trunc().rem_euclid(4_294_967_296.0) as usize
        } else {
            0
        };
        parts.truncate(limit);
    }
    Ok(Value::arr(parts))
}

fn repeat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let n = to_num(&arg(args, 0));
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    if n < 0.0 || n.is_infinite() {
        let text = to_str(&Value::Num(n));
        return Err(vm.make_error("RangeError", &format!("Invalid count value: {}", text)));
    }
    let s = this_str(&this);
    if s.is_empty() {
        return Ok(Value::str(""));
    }
    if n * char_len(&s) as f64 > MAX_STRING_LEN as f64 {
        return Err(vm.make_error("RangeError", "Invalid string length"));
    }
    Ok(Value::str(&s.repeat(n as usize)))
}

fn replace(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    Ok(Value::str(&s.replacen(
        &str_arg(args, 0),
        &str_arg(args, 1),
        1,
    )))
}

/// Pads to `width` characters, repeating `fill` at the start (or end) of the string.
fn pad(vm: &mut Vm, this: &Value, args: &[Value], at_start: bool) -> Result<Value, Value> {
    let s = this_str(this);
    let width = to_num(&arg(args, 0));
    let width = if width.is_nan() { 0.0 } else { width.trunc() };
    let fill = match arg(args, 1) {
        Value::Undef => " ".to_string(),
        v => to_str(&v),
    };
    let len = char_len(&s);
    if width <= len as f64 || fill.is_empty() {
        return Ok(Value::str(&s));
    }
    if width > MAX_STRING_LEN as f64 {
        return Err(vm.make_error("RangeError", "Invalid string length"));
    }
    let padding: String = fill.chars().cycle().take(width as usize - len).collect();
    let text = if at_start { padding + &s } else { s + &padding };
    Ok(Value::str(&text))
}

fn pad_start(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    pad(vm, &this, args, true)
}

fn pad_end(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    pad(vm, &this, args, false)
}

fn concat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let mut s = this_str(&this);
    for v in args {
        s.push_str(&to_str(v));
        if s.len() > MAX_STRING_LEN {
            return Err(vm.make_error("RangeError", "Invalid string length"));
        }
    }
    Ok(Value::str(&s))
}

fn to_string(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&this_str(&this)))
}

fn to_fixed(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let digits = to_num(&arg(args, 0));
    let digits = if digits.is_nan() {
        0
    } else {
        digits.clamp(0.0, 100.0) as usize
    };
    Ok(Value::str(&to_fixed_text(to_num(&this), digits)))
}

/// `toFixed` rounds half up on the scaled value (`(2.5).toFixed(0)` is "3"), unlike `{:.n}`.
fn to_fixed_text(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return num_to_string(x);
    }
    let scaled = (x.abs() * 10f64.powi(digits as i32)).round();
    let text = format!("{:0>width$}", format!("{:.0}", scaled), width = digits + 1);
    let (int_part, frac_part) = text.split_at(text.len() - digits);
    let body = if digits == 0 {
        int_part.to_string()
    } else {
        format!("{}.{}", int_part, frac_part)
    };
    if x < 0.0 {
        format!("-{}", body)
    } else {
        body
    }
}

fn number_to_string(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&num_to_string(to_num(&this))))
}
