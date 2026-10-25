//! Runtime values and the conversion and comparison rules that operate on them.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use crate::bytecode::Proto;
use crate::vm::Vm;

pub type Props = Vec<(Rc<str>, Value)>;
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

#[derive(Clone)]
pub enum Value {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<String>),
    Arr(ArrRef),
    Obj(ObjRef),
    Func(Rc<Closure>),
    Native(&'static NativeDef),
    /// A boxed local. It only ever sits in a frame slot and is never visible to user code.
    Cell(Rc<RefCell<Value>>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::new(s.to_string()))
    }

    pub fn arr(items: Vec<Value>) -> Value {
        Value::Arr(Rc::new(RefCell::new(items)))
    }

    pub fn obj() -> Value {
        Value::obj_with(0)
    }

    pub fn obj_with(capacity: usize) -> Value {
        Value::Obj(Rc::new(RefCell::new(Vec::with_capacity(capacity))))
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
    match props.iter().position(|(k, _)| &**k == name) {
        Some(i) => props[i].1 = v,
        None => props.push((Rc::from(name), v)),
    }
}

/// Sets an own property using a key that is already shared, so no string is copied.
pub fn obj_put(obj: &ObjRef, key: &Rc<str>, v: Value) {
    let mut props = obj.borrow_mut();
    match props.iter().position(|(k, _)| **k == **key) {
        Some(i) => props[i].1 = v,
        None => props.push((key.clone(), v)),
    }
}

/// Reads an own property of a plain object.
pub fn obj_get(obj: &ObjRef, name: &str) -> Option<Value> {
    obj.borrow()
        .iter()
        .find(|(k, _)| &**k == name)
        .map(|(_, v)| v.clone())
}

/// Builds a plain object from name/value pairs.
pub fn make_obj(pairs: Vec<(&str, Value)>) -> Value {
    let props: Props = pairs.into_iter().map(|(k, v)| (Rc::from(k), v)).collect();
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

pub fn to_num(v: &Value) -> f64 {
    match v {
        Value::Undef => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Num(n) => *n,
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
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

/// Formats a number the way JavaScript prints it for the common cases.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else if n == 0.0 {
        "0".to_string()
    } else if n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else if n.abs() >= 1e21 || n.abs() < 1e-6 {
        exponent_form(n)
    } else {
        format!("{}", n)
    }
}

/// JavaScript's exponent notation, such as `1e+21` or `1.5e-7`.
fn exponent_form(n: f64) -> String {
    let text = format!("{:e}", n);
    match text.split_once('e') {
        Some((mantissa, exp)) if exp.starts_with('-') => format!("{}e{}", mantissa, exp),
        Some((mantissa, exp)) => format!("{}e+{}", mantissa, exp),
        None => text,
    }
}

pub fn to_str(v: &Value) -> String {
    match v {
        Value::Undef => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_string(*n),
        Value::Str(s) => s.to_string(),
        Value::Arr(a) => a
            .borrow()
            .iter()
            .map(|item| {
                if item.is_nullish() {
                    String::new()
                } else {
                    to_str(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Obj(_) => "[object Object]".to_string(),
        Value::Func(_) | Value::Native(_) => "function".to_string(),
        Value::Cell(c) => to_str(&c.borrow()),
    }
}

/// The `+` operator: numeric addition, or string concatenation when either side is not a number.
pub fn add(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
        (Value::Str(x), Value::Str(y)) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Value::str(&s)
        }
        _ if is_textual(a) || is_textual(b) => Value::str(&(to_str(a) + &to_str(b))),
        _ => Value::Num(to_num(a) + to_num(b)),
    }
}

/// `old + rhs` where `old` is owned. A string held only by `old` is extended in place,
/// which keeps loops that build a string linear instead of quadratic.
pub fn concat_owned(old: Value, rhs: Value) -> Value {
    match old {
        Value::Str(mut s) => {
            if Rc::get_mut(&mut s).is_none() {
                let mut copy = String::with_capacity(s.len() * 2 + 16);
                copy.push_str(&s);
                s = Rc::new(copy);
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
        (ai % bi) as f64
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
        _ => to_num(a).partial_cmp(&to_num(b)),
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
        _ => strict_eq(a, b),
    }
}
