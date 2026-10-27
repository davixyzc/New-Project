//! String and number methods. Strings are UTF-8; indices count characters, not bytes.

use std::rc::Rc;

use crate::builtins::arg;
use crate::builtins::array::bounds;
use crate::value::{num_to_string, to_num, to_str, JsStr, NativeDef, Value, MAX_STRING_LEN};
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
    NativeDef {
        name: "at",
        func: at_method,
    },
    NativeDef {
        name: "codePointAt",
        func: code_point_at,
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

static METHOD_INDEX: std::sync::OnceLock<Vec<&'static NativeDef>> = std::sync::OnceLock::new();

pub fn method(name: &str) -> Option<&'static NativeDef> {
    crate::builtins::sorted_lookup(METHODS, &METHOD_INDEX, name)
}

/// `method`, which starts from the position in `hint` (see `sorted_lookup_hinted`).
pub fn method_hinted(name: &str, hint: &std::cell::Cell<u32>) -> Option<&'static NativeDef> {
    crate::builtins::sorted_lookup_hinted(METHODS, &METHOD_INDEX, name, hint)
}

pub fn number_method(name: &str) -> Option<&'static NativeDef> {
    NUMBER_METHODS.iter().find(|d| d.name == name)
}

/// Length in characters.
pub fn char_len(s: &JsStr) -> usize {
    s.char_count()
}

/// The character at a character index.
pub fn char_at(s: &JsStr, i: usize) -> Option<char> {
    if s.is_ascii() {
        s.as_bytes().get(i).map(|b| *b as char)
    } else {
        s.chars().nth(i)
    }
}

/// The receiver as a string. A string receiver is shared, not copied, so a loop over `s.charCodeAt(i)` costs
/// the same per call whatever the length of `s`.
fn this_str(this: &Value) -> Rc<JsStr> {
    match this {
        Value::Str(s) => Rc::clone(s),
        other => Rc::new(JsStr::new(to_str(other))),
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
    Ok(match i.and_then(|i| char_at(&s, i)) {
        Some(c) => crate::value::char_value(c),
        None => Value::str(""),
    })
}

fn char_code_at(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    Ok(Value::Num(char_code(&s, to_num(&arg(args, 0)))))
}

/// `s.charCodeAt(i)` for a number `i`: the code unit at `i`, or NaN when there is none.
pub fn char_code(s: &JsStr, i: f64) -> f64 {
    index_arg(&Value::Num(i))
        .and_then(|i| char_at(s, i))
        .map_or(f64::NAN, |c| c as u32 as f64)
}

/// The built-in `charCodeAt`, which native code recognises by identity.
pub fn char_code_at_def() -> &'static NativeDef {
    METHODS
        .iter()
        .find(|m| m.name == "charCodeAt")
        .expect("charCodeAt is a string method")
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

/// The text between two character positions, which are in range. ASCII text is cut by byte, so a short slice of a
/// long string costs only its own length.
fn char_range(s: &JsStr, from: usize, to: usize) -> &str {
    if s.is_ascii() {
        &s[from..to]
    } else {
        &s[byte_offset(s, from)..byte_offset(s, to)]
    }
}

fn slice(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let (from, to) = bounds(char_len(&s), &arg(args, 0), &arg(args, 1));
    Ok(Value::str(char_range(&s, from, to)))
}

fn substring(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let len = char_len(&s) as f64;
    let clamp = |v: f64| if v.is_nan() { 0.0 } else { v.clamp(0.0, len) } as usize;
    let a = clamp(to_num(&arg(args, 0)));
    let b = match arg(args, 1) {
        Value::Undef => len as usize,
        v => clamp(to_num(&v)),
    };
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    Ok(Value::str(char_range(&s, from, to)))
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

/// The pieces of `s` between the occurrences of the ASCII byte `sep`, as `str::split` gives them.
fn split_at_byte(s: &str, sep: u8) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        if b == sep {
            parts.push(Value::str(&s[start..i]));
            start = i + 1;
        }
    }
    parts.push(Value::str(&s[start..]));
    parts
}

fn split(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let mut parts: Vec<Value> = match arg(args, 0) {
        Value::Undef => vec![Value::str(&s)],
        sep => {
            let sep = to_str(&sep);
            match sep.as_bytes() {
                [] => s.chars().map(|c| Value::str(&c.to_string())).collect(),
                // An ASCII byte never occurs inside a multi-byte character, so the pieces can be cut at it directly.
                [byte] if byte.is_ascii() => split_at_byte(&s, *byte),
                _ => s.split(sep.as_str()).map(Value::str).collect(),
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
    let text = if at_start {
        format!("{padding}{s}")
    } else {
        format!("{s}{padding}")
    };
    Ok(Value::from_string(text))
}

fn pad_start(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    pad(vm, &this, args, true)
}

fn pad_end(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    pad(vm, &this, args, false)
}

fn concat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let mut s = this_str(&this).to_string();
    for v in args {
        s.push_str(&to_str(v));
        if s.len() > MAX_STRING_LEN {
            return Err(vm.make_error("RangeError", "Invalid string length"));
        }
    }
    Ok(Value::from_string(s))
}

/// `at(index)`: the character at `index`, counting a negative index from the end. Outside the string is undefined.
fn at_method(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    let len = char_len(&s) as f64;
    let n = to_num(&arg(args, 0));
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let i = if n < 0.0 { len + n } else { n };
    if i < 0.0 || i >= len {
        return Ok(Value::Undef);
    }
    Ok(char_at(&s, i as usize).map_or(Value::Undef, crate::value::char_value))
}

/// `codePointAt(index)`: the code point of the character at `index`, or undefined outside the string.
fn code_point_at(_vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let s = this_str(&this);
    Ok(
        match index_arg(&arg(args, 0)).and_then(|i| char_at(&s, i)) {
            Some(c) => Value::Num(f64::from(u32::from(c))),
            None => Value::Undef,
        },
    )
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

fn number_to_string(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let n = to_num(&this);
    let radix = match arg(args, 0) {
        Value::Undef => 10.0,
        r => to_num(&r).trunc(),
    };
    if !(2.0..=36.0).contains(&radix) {
        return Err(vm.make_error(
            "RangeError",
            "toString() radix argument must be between 2 and 36",
        ));
    }
    if radix == 10.0 {
        return Ok(Value::from_string(num_to_string(n)));
    }
    Ok(Value::str(&num_to_radix(n, radix as u32)))
}

/// `Number.prototype.toString(radix)` for a radix other than 10, as V8 writes it: the integer digits are exact, and
/// the fraction gets digits only until they name the number to within half the gap to the next double.
fn num_to_radix(value: f64, radix: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let base = f64::from(radix);
    let magnitude = value.abs();
    let mut integer = magnitude.floor();
    let mut fraction = magnitude - integer;
    let mut delta =
        (0.5 * (f64::from_bits(magnitude.to_bits() + 1) - magnitude)).max(f64::from_bits(1));
    let mut frac_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= base;
            delta *= base;
            let digit = fraction as u32;
            frac_digits.push(DIGITS[digit as usize]);
            fraction -= f64::from(digit);
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying through the digits that are already written.
                loop {
                    match frac_digits.pop() {
                        None => {
                            integer += 1.0;
                            break;
                        }
                        Some(c) => {
                            let d = if c > b'9' {
                                u32::from(c - b'a') + 10
                            } else {
                                u32::from(c - b'0')
                            };
                            if d + 1 < radix {
                                frac_digits.push(DIGITS[(d + 1) as usize]);
                                break;
                            }
                        }
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Digits a double cannot tell apart (beyond 2^53) are written as zeros, as V8 does.
    let mut int_digits: Vec<u8> = Vec::new();
    while integer / base >= 9_007_199_254_740_992.0 {
        integer /= base;
        int_digits.push(b'0');
    }
    loop {
        let remainder = integer % base;
        int_digits.push(DIGITS[remainder as usize]);
        integer = (integer - remainder) / base;
        if integer <= 0.0 {
            break;
        }
    }
    int_digits.reverse();
    let mut text = String::new();
    if value < 0.0 {
        text.push('-');
    }
    text.push_str(std::str::from_utf8(&int_digits).unwrap_or("0"));
    if !frac_digits.is_empty() {
        text.push('.');
        text.push_str(std::str::from_utf8(&frac_digits).unwrap_or(""));
    }
    text
}
