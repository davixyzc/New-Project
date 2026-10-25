//! Runtime values and the conversion and comparison rules that operate on them.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::bytecode::Proto;
use crate::vm::Vm;

/// The own properties of an object, in the order they were added. An object with many properties also keeps an
/// index from name to position, so that a lookup does not scan them.
#[derive(Clone, Default)]
pub struct Props {
    items: Vec<(Rc<str>, Value)>,
    index: Option<NameMap<usize>>,
}

/// From this many properties on, an object gets an index by name.
const INDEX_FROM: usize = 16;

impl Props {
    pub fn with_capacity(capacity: usize) -> Props {
        Props {
            items: Vec::with_capacity(capacity),
            index: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (Rc<str>, Value)> {
        self.items.iter()
    }

    pub fn value(&self, i: usize) -> &Value {
        &self.items[i].1
    }

    pub fn set_value(&mut self, i: usize, v: Value) {
        self.items[i].1 = v;
    }

    /// The position of the property called `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(name).copied(),
            None => self.items.iter().position(|(k, _)| &**k == name),
        }
    }

    /// The position of the property with the shared key `key`. The pointer is compared first, which finds almost
    /// every key, and the text only when the pointer finds nothing.
    pub fn position_of_key(&self, key: &Rc<str>) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(&**key).copied(),
            None => self
                .items
                .iter()
                .position(|(k, _)| Rc::ptr_eq(k, key))
                .or_else(|| self.items.iter().position(|(k, _)| **k == **key)),
        }
    }

    /// The position of `key`, looked up at `hint` first. The hint is set to the position that the lookup finds, so
    /// a site that keeps seeing objects of one shape finds the key without a scan.
    pub fn position_hinted(&self, key: &Rc<str>, hint: &Cell<u32>) -> Option<usize> {
        let at = hint.get() as usize;
        if self.items.get(at).is_some_and(|(k, _)| Rc::ptr_eq(k, key)) {
            return Some(at);
        }
        let i = self.position_of_key(key)?;
        hint.set(i as u32);
        Some(i)
    }

    /// Adds a property, which the caller has checked is not there yet.
    pub fn push(&mut self, key: Rc<str>, v: Value) {
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.items.len());
        }
        self.items.push((key, v));
        if self.index.is_none() && self.items.len() > INDEX_FROM {
            self.index = Some(
                self.items
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (k.clone(), i))
                    .collect(),
            );
        }
    }
}

impl std::ops::Index<usize> for Props {
    type Output = (Rc<str>, Value);

    fn index(&self, i: usize) -> &(Rc<str>, Value) {
        &self.items[i]
    }
}

pub type ObjRef = Rc<RefCell<Props>>;
pub type ArrRef = Rc<RefCell<Vec<Value>>>;
/// Arguments arrive as a borrowed slice, so calls do not allocate.
pub type NativeFn = fn(&mut Vm, Value, &[Value]) -> Result<Value, Value>;

/// A built-in function. Lives in a `static` table, so values can point at it directly.
pub struct NativeDef {
    pub name: &'static str,
    pub func: NativeFn,
}

/// A user function together with the variables it captured.
pub struct Closure {
    pub proto: Rc<Proto>,
    pub upvals: Vec<Rc<RefCell<Value>>>,
    /// `this` captured at creation time; only used by arrow functions.
    pub this: Value,
}

/// The text of a string value. It keeps its character count and whether every character is ASCII, so `length` and
/// indexing take constant time for ASCII text, which is the usual case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct JsStr {
    text: String,
    chars: usize,
    ascii: bool,
}

impl JsStr {
    pub fn new(text: String) -> JsStr {
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        JsStr { text, chars, ascii }
    }

    /// The length in characters.
    pub fn char_count(&self) -> usize {
        self.chars
    }

    /// Whether every character is ASCII, so that a character index is also a byte index.
    pub fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// Appends `more`, keeping the counts up to date.
    pub fn push_str(&mut self, more: &str) {
        self.chars += if more.is_ascii() {
            more.len()
        } else {
            more.chars().count()
        };
        self.ascii = self.ascii && more.is_ascii();
        self.text.push_str(more);
    }
}

impl std::ops::Deref for JsStr {
    type Target = String;

    fn deref(&self) -> &String {
        &self.text
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// The one-character string for `c`. ASCII characters share cached values, so indexing a string does not allocate.
pub fn char_value(c: char) -> Value {
    thread_local! {
        static ASCII: Vec<Rc<JsStr>> = (0u8..128)
            .map(|b| Rc::new(JsStr::new((b as char).to_string())))
            .collect();
    }
    if c.is_ascii() {
        ASCII.with(|table| Value::Str(Rc::clone(&table[c as usize])))
    } else {
        Value::str(c.encode_utf8(&mut [0; 4]))
    }
}

#[derive(Clone)]
#[repr(u8)]
pub enum Value {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<JsStr>),
    Arr(ArrRef),
    Obj(ObjRef),
    Func(Rc<Closure>),
    Native(&'static NativeDef),
    /// A boxed local. It only ever sits in a frame slot and is never visible to user code.
    Cell(Rc<RefCell<Value>>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::new(JsStr::new(s.to_string())))
    }

    /// A string value that takes over `s`, without copying it.
    pub fn from_string(s: String) -> Value {
        Value::Str(Rc::new(JsStr::new(s)))
    }

    pub fn arr(items: Vec<Value>) -> Value {
        Value::Arr(Rc::new(RefCell::new(items)))
    }

    pub fn obj() -> Value {
        Value::obj_with(0)
    }

    pub fn obj_with(capacity: usize) -> Value {
        Value::Obj(Rc::new(RefCell::new(Props::with_capacity(capacity))))
    }

    pub fn is_object(&self) -> bool {
        matches!(
            self,
            Value::Obj(_) | Value::Arr(_) | Value::Func(_) | Value::Native(_)
        )
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undef | Value::Null)
    }

    /// True when dropping the value releases a reference. Scalars need no drop at all.
    #[inline(always)]
    pub fn owns_ref(&self) -> bool {
        matches!(
            self,
            Value::Str(_) | Value::Arr(_) | Value::Obj(_) | Value::Func(_) | Value::Cell(_)
        )
    }
}

/// Sets (or replaces) an own property on a plain object.
pub fn obj_set(obj: &ObjRef, name: &str, v: Value) {
    let mut props = obj.borrow_mut();
    match props.position(name) {
        Some(i) => props.set_value(i, v),
        None => props.push(Rc::from(name), v),
    }
}

/// Sets an own property using a key that is already shared, so no string is copied.
pub fn obj_put(obj: &ObjRef, key: &Rc<str>, v: Value) {
    let mut props = obj.borrow_mut();
    match props.position_of_key(key) {
        Some(i) => props.set_value(i, v),
        None => props.push(key.clone(), v),
    }
}

/// Reads an own property using a shared key, trying the position in `hint` first (see `Props::position_hinted`).
/// Names from the same source share one allocation (see `intern_name`), so the pointer decides almost every lookup.
pub fn obj_get_hinted(obj: &ObjRef, key: &Rc<str>, hint: &Cell<u32>) -> Option<Value> {
    let props = obj.borrow();
    props
        .position_hinted(key, hint)
        .map(|i| props.value(i).clone())
}

/// `obj_put`, which tries the position in `hint` first (see `Props::position_hinted`).
pub fn obj_put_hinted(obj: &ObjRef, key: &Rc<str>, v: Value, hint: &Cell<u32>) {
    let mut props = obj.borrow_mut();
    match props.position_hinted(key, hint) {
        Some(i) => props.set_value(i, v),
        None => props.push(key.clone(), v),
    }
}

/// A hasher for names, which are short: a rotate and a multiply per byte. The default hasher is built for long keys
/// and is slow on short ones, and a built-in such as `Math` is looked up by name each time it is used.
#[derive(Default)]
pub struct NameHasher(u64);

impl std::hash::Hasher for NameHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
}

/// A table keyed by name, using the name hasher.
pub type NameMap<V> = HashMap<Rc<str>, V, std::hash::BuildHasherDefault<NameHasher>>;

thread_local! {
    static NAMES: RefCell<std::collections::HashMap<Box<str>, Rc<str>>> = RefCell::new(Default::default());
}

/// The shared allocation for a name in the source, so that every use of `name` is the same `Rc`, and a property
/// lookup by pointer finds it. Only names in the source are interned: their number is bounded by the program text.
pub fn intern_name(name: &str) -> Rc<str> {
    NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some(shared) = names.get(name) {
            return shared.clone();
        }
        let shared: Rc<str> = Rc::from(name);
        names.insert(Box::from(name), shared.clone());
        shared
    })
}

/// Reads an own property of a plain object.
pub fn obj_get(obj: &ObjRef, name: &str) -> Option<Value> {
    let props = obj.borrow();
    props.position(name).map(|i| props.value(i).clone())
}

/// Builds a plain object from name/value pairs.
pub fn make_obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut props = Props::default();
    for (k, v) in pairs {
        props.push(intern_name(k), v);
    }
    Value::Obj(Rc::new(RefCell::new(props)))
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Undef | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undef => "undefined",
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Arr(_) | Value::Obj(_) | Value::Cell(_) => "object",
        Value::Func(_) | Value::Native(_) => "function",
    }
}

/// `ToInt32`: the number as the 32-bit integer that the bitwise operators work on. Infinities and NaN are 0, and
/// anything else is truncated and wrapped modulo 2^32.
pub fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4_294_967_296.0) as u32) as i32
}

pub fn to_num(v: &Value) -> f64 {
    match v {
        Value::Undef => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Num(n) => *n,
        Value::Str(s) => text_to_num(s),
        Value::Arr(a) => {
            let a = a.borrow();
            match a.len() {
                0 => 0.0,
                1 => to_num(&a[0]),
                _ => f64::NAN,
            }
        }
        _ => f64::NAN,
    }
}

/// Formats a number the way JavaScript's `Number::toString` does: the shortest digits that read back as the same
/// number, in positional form up to 21 digits and in exponent form beyond that.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        let mut text = String::with_capacity(20);
        push_int(&mut text, n as i64);
        return text;
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let (digits, point) = shortest_digits(n.abs());
    let k = digits.len() as i32;
    let body = if k <= point && point <= 21 {
        format!("{}{}", digits, "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else if -6 < point && point <= 0 {
        format!("0.{}{}", "0".repeat((-point) as usize), digits)
    } else {
        let e = point - 1;
        let exp_sign = if e < 0 { "-" } else { "+" };
        if k == 1 {
            format!("{}e{}{}", digits, exp_sign, e.abs())
        } else {
            format!("{}.{}e{}{}", &digits[..1], &digits[1..], exp_sign, e.abs())
        }
    };
    format!("{}{}", sign, body)
}

/// The shortest digits that read back as `x`, which is positive and finite, and the position of the decimal point:
/// `x` is `0.d1d2...dk` times ten to the power `point`. Rust rounds an exact expansion to the even digit when two
/// candidates of that length are equally close, which is the one JavaScript picks, so that rounding is used when
/// it still reads back as `x`.
fn shortest_digits(x: f64) -> (String, i32) {
    let short = format!("{:e}", x);
    let (mantissa, exp) = short.split_once('e').unwrap_or((short.as_str(), "0"));
    let mut digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut exp10: i32 = exp.parse().unwrap_or(0);
    if could_tie(x) {
        let exact = format!("{:.*e}", digits.len() - 1, x);
        if let Some((m, e)) = exact.split_once('e') {
            let alt: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
            if alt != digits && exact.parse::<f64>() == Ok(x) {
                digits = alt;
                exp10 = e.parse().unwrap_or(exp10);
            }
        }
    }
    (digits.trim_end_matches('0').to_string(), exp10 + 1)
}

/// Whether two shortest candidates of `x` can tie. A tie needs the exact decimal form of `x` to have one more
/// significant digit than the candidates, which are at most 17 digits, so an exact form of more than 18 digits
/// rules it out. A double `m * 2^e` with `e < 0` has the exact form `m * 5^-e` times `10^e`.
fn could_tie(x: f64) -> bool {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let zeros = m.trailing_zeros();
    m >>= zeros;
    e += zeros as i32;
    if e >= 0 {
        return false;
    }
    let j = (-e) as u32;
    // `m` is below 2^53, so `m * 5^27` still fits in a u128.
    j <= 27 && (u128::from(m) * 5u128.pow(j)).to_string().len() <= 18
}

pub fn to_str(v: &Value) -> String {
    to_str_in(v, None)
}

/// `to_str`, where `joining` is the chain of arrays whose text is still being built further out.
fn to_str_in(v: &Value, joining: Option<&Joining>) -> String {
    match v {
        Value::Undef => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_string(*n),
        Value::Str(s) => s.to_string(),
        Value::Arr(a) => join_items(a, ",", joining),
        Value::Obj(o) => error_text(o).unwrap_or_else(|| "[object Object]".to_string()),
        Value::Func(_) | Value::Native(_) => "function".to_string(),
        Value::Cell(c) => to_str_in(&c.borrow(), joining),
    }
}

/// An array whose text is being built, and the arrays it sits in. Each one lives on the stack of its `join_items` call.
pub struct Joining<'a> {
    id: *const RefCell<Vec<Value>>,
    outer: Option<&'a Joining<'a>>,
}

/// Joins the items of an array with `sep`; a null or undefined item is empty. An array that is already being joined
/// further out is empty too, so an array that contains itself ends there, as in `Array.prototype.join`.
pub fn join_items(a: &ArrRef, sep: &str, outer: Option<&Joining>) -> String {
    let id = Rc::as_ptr(a);
    let mut link = outer;
    while let Some(j) = link {
        if j.id == id {
            return String::new();
        }
        link = j.outer;
    }
    let joining = Joining { id, outer };
    let items = a.borrow();
    let mut text = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            text.push_str(sep);
        }
        match item {
            Value::Undef | Value::Null => {}
            Value::Str(s) => text.push_str(s),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => push_int(&mut text, *n as i64),
            other => text.push_str(&to_str_in(other, Some(&joining))),
        }
    }
    text
}

/// `Number(text)` for a string: decimal numbers, `0x`, `0o` and `0b` integers, and the names `Infinity` and
/// `-Infinity`. Anything else, such as `inf` or `nan`, is not a number.
fn text_to_num(text: &str) -> f64 {
    let t = text.trim();
    if t.is_empty() {
        return 0.0;
    }
    let radix: u32 = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 0,
    };
    if radix != 0 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return digits.chars().fold(0.0, |acc, c| {
            acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
        });
    }
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if t
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E')) =>
        {
            t.parse::<f64>().unwrap_or(f64::NAN)
        }
        _ => f64::NAN,
    }
}

/// `String(error)` for an error object, which `make_error` gives `name`, `message` and `stack`. Any other object
/// prints as `[object Object]`.
fn error_text(o: &ObjRef) -> Option<String> {
    let props = o.borrow();
    let get = |key: &str| props.iter().find(|(k, _)| &**k == key).map(|(_, v)| v);
    get("stack")?;
    let name = to_str(get("name")?);
    let message = to_str(get("message")?);
    Some(match (name.is_empty(), message.is_empty()) {
        (_, true) => name,
        (true, false) => message,
        (false, false) => format!("{}: {}", name, message),
    })
}

/// The positions of an object's properties in the order JavaScript lists them: keys that are array indices come
/// first, in ascending order, and the others keep their insertion order.
pub fn key_order(props: &Props) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut names = Vec::new();
    for (i, (k, _)) in props.iter().enumerate() {
        match array_index(k) {
            Some(n) => indices.push((n, i)),
            None => names.push(i),
        }
    }
    indices.sort_unstable();
    indices.into_iter().map(|(_, i)| i).chain(names).collect()
}

/// An array index spelled the way JavaScript spells it: digits with no leading zero, below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    (n < u64::from(u32::MAX)).then_some(n as u32)
}

/// The `+` operator: numeric addition, or string concatenation when either side is not a number.
pub fn add(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
        (Value::Str(x), Value::Str(y)) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Value::from_string(s)
        }
        _ if is_textual(a) || is_textual(b) => {
            let mut s = String::new();
            append_text(&mut s, a);
            append_text(&mut s, b);
            Value::from_string(s)
        }
        _ => Value::Num(to_num(a) + to_num(b)),
    }
}

/// Appends the text of `v`, which is how `+` and template strings read a value, to `s`. Numbers that are whole are
/// written straight into `s`, without a temporary string.
fn append_text(s: &mut String, v: &Value) {
    match v {
        Value::Str(x) => s.push_str(x),
        Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => push_int(s, *n as i64),
        other => s.push_str(&to_str(other)),
    }
}

/// Appends the decimal digits of `v`.
fn push_int(s: &mut String, v: i64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = v.unsigned_abs();
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if v < 0 {
        s.push('-');
    }
    // The digits are ASCII, so they are valid UTF-8.
    s.push_str(std::str::from_utf8(&digits[at..]).unwrap_or("0"));
}

/// The longest text the string operations build, in bytes. It is V8's limit, which V8 counts in characters.
pub const MAX_STRING_LEN: usize = (1 << 29) - 24;

/// Whether joining `a` and `b` would give a string over `MAX_STRING_LEN`. Only string operands are measured,
/// since their size is known without converting them.
pub fn joins_too_long(a: &Value, b: &Value) -> bool {
    let size = |v: &Value| match v {
        Value::Str(s) => s.len(),
        _ => 0,
    };
    size(a) + size(b) > MAX_STRING_LEN
}

/// `old + rhs` where `old` is owned. A string held only by `old` is extended in place,
/// which keeps loops that build a string linear instead of quadratic.
pub fn concat_owned(old: Value, rhs: Value) -> Value {
    match old {
        Value::Str(mut s) => {
            if Rc::get_mut(&mut s).is_none() {
                let mut copy = String::with_capacity(s.len() * 2 + 16);
                copy.push_str(&s);
                s = Rc::new(JsStr::new(copy));
            }
            let buf = Rc::get_mut(&mut s).unwrap();
            match &rhs {
                Value::Str(r) => buf.push_str(r),
                other => buf.push_str(&to_str(other)),
            }
            Value::Str(s)
        }
        other => add(&other, &rhs),
    }
}

/// `%` with an integer fast path, because the floating-point remainder is slow.
pub fn js_mod(a: f64, b: f64) -> f64 {
    // Casts avoid the libm call that `fract` needs on baseline x86-64.
    let (ai, bi) = (a as i64, b as i64);
    const LIMIT: u64 = 1 << 53;
    if ai as f64 == a
        && bi as f64 == b
        && bi != 0
        && ai.unsigned_abs() < LIMIT
        && bi.unsigned_abs() < LIMIT
    {
        let r = ai % bi;
        // A zero remainder takes the sign of the dividend, so `-4 % 2` is -0.
        if r == 0 && a.is_sign_negative() {
            -0.0
        } else {
            r as f64
        }
    } else {
        a % b
    }
}

fn is_textual(v: &Value) -> bool {
    matches!(
        v,
        Value::Str(_) | Value::Arr(_) | Value::Obj(_) | Value::Func(_) | Value::Native(_)
    )
}

/// Orders two values for `<`, `<=`, `>` and `>=`. `None` means the comparison is false (NaN).
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ if a.is_object() || b.is_object() => compare(&to_primitive(a), &to_primitive(b)),
        _ => to_num(a).partial_cmp(&to_num(b)),
    }
}

/// Objects, arrays and functions take their text, as `ToPrimitive` does before a relational comparison.
fn to_primitive(v: &Value) -> Value {
    if v.is_object() {
        Value::str(&to_str(v))
    } else {
        v.clone()
    }
}

pub fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undef, Value::Undef) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Arr(x), Value::Arr(y)) => Rc::ptr_eq(x, y),
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => std::ptr::eq(*x, *y),
        _ => false,
    }
}

/// The `==` operator, with the common coercions between primitives.
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undef | Value::Null, Value::Undef | Value::Null) => true,
        (Value::Undef | Value::Null, _) | (_, Value::Undef | Value::Null) => false,
        (Value::Str(x), Value::Str(y)) => x == y,
        (
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
        ) => to_num(a) == to_num(b),
        (
            Value::Arr(_) | Value::Obj(_) | Value::Func(_),
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
        ) => loose_eq(&Value::str(&to_str(a)), b),
        (
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
            Value::Arr(_) | Value::Obj(_) | Value::Func(_),
        ) => loose_eq(a, &Value::str(&to_str(b))),
        _ => strict_eq(a, b),
    }
}
