//! Global functions, the Object/Array/Date/Math/process namespaces, and `require`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::builtins::{arg, fs, table_object};
use crate::value::{key_order, obj_set, to_num, to_str, truthy, NativeDef, Value};
use crate::vm::Vm;

pub static FUNCTIONS: &[NativeDef] = &[
    NativeDef {
        name: "Error",
        func: error_ctor,
    },
    NativeDef {
        name: "TypeError",
        func: type_error_ctor,
    },
    NativeDef {
        name: "RangeError",
        func: range_error_ctor,
    },
    NativeDef {
        name: "SyntaxError",
        func: syntax_error_ctor,
    },
    NativeDef {
        name: "ReferenceError",
        func: reference_error_ctor,
    },
    NativeDef {
        name: "Number",
        func: number,
    },
    NativeDef {
        name: "String",
        func: string,
    },
    NativeDef {
        name: "Boolean",
        func: boolean,
    },
    NativeDef {
        name: "parseInt",
        func: parse_int,
    },
    NativeDef {
        name: "parseFloat",
        func: parse_float,
    },
    NativeDef {
        name: "isNaN",
        func: is_nan,
    },
    NativeDef {
        name: "isFinite",
        func: is_finite,
    },
    NativeDef {
        name: "setTimeout",
        func: set_timeout,
    },
    NativeDef {
        name: "setInterval",
        func: set_interval,
    },
    NativeDef {
        name: "setImmediate",
        func: set_immediate,
    },
    NativeDef {
        name: "clearTimeout",
        func: clear_timer,
    },
    NativeDef {
        name: "clearInterval",
        func: clear_timer,
    },
    NativeDef {
        name: "clearImmediate",
        func: clear_timer,
    },
];

pub static OBJECT: &[NativeDef] = &[
    NativeDef {
        name: "keys",
        func: object_keys,
    },
    NativeDef {
        name: "values",
        func: object_values,
    },
    NativeDef {
        name: "entries",
        func: object_entries,
    },
];

pub static ARRAY: &[NativeDef] = &[
    NativeDef {
        name: "isArray",
        func: is_array,
    },
    NativeDef {
        name: "from",
        func: array_from,
    },
];

pub static DATE: &[NativeDef] = &[NativeDef {
    name: "now",
    func: date_now,
}];

pub static PROCESS: &[NativeDef] = &[
    NativeDef {
        name: "exit",
        func: process_exit,
    },
    NativeDef {
        name: "cwd",
        func: process_cwd,
    },
    NativeDef {
        name: "nextTick",
        func: next_tick,
    },
];

pub static MATH: &[NativeDef] = &[
    NativeDef {
        name: "floor",
        func: floor,
    },
    NativeDef {
        name: "ceil",
        func: ceil,
    },
    NativeDef {
        name: "round",
        func: round,
    },
    NativeDef {
        name: "trunc",
        func: trunc,
    },
    NativeDef {
        name: "abs",
        func: abs,
    },
    NativeDef {
        name: "sqrt",
        func: sqrt,
    },
    NativeDef {
        name: "pow",
        func: pow,
    },
    NativeDef {
        name: "min",
        func: min,
    },
    NativeDef {
        name: "max",
        func: max,
    },
    NativeDef {
        name: "sign",
        func: sign,
    },
    NativeDef {
        name: "log",
        func: log,
    },
    NativeDef {
        name: "exp",
        func: exp,
    },
    NativeDef {
        name: "sin",
        func: sin,
    },
    NativeDef {
        name: "cos",
        func: cos,
    },
    NativeDef {
        name: "tan",
        func: tan,
    },
    NativeDef {
        name: "random",
        func: random,
    },
];

/// `require` is also reachable as a global; modules get their own copy as a parameter.
pub static REQUIRE: NativeDef = NativeDef {
    name: "require",
    func: require,
};

pub fn require_fn() -> Value {
    Value::Native(&REQUIRE)
}

pub fn process_object(argv: Vec<String>) -> Value {
    let process = table_object(PROCESS);
    let env = Value::obj();
    if let Value::Obj(o) = &env {
        for (k, v) in std::env::vars() {
            obj_set(o, &k, Value::str(&v));
        }
    }
    if let Value::Obj(o) = &process {
        obj_set(
            o,
            "argv",
            Value::arr(argv.iter().map(|s| Value::str(s)).collect()),
        );
        obj_set(o, "env", env);
        obj_set(o, "platform", Value::str("linux"));
        obj_set(
            o,
            "version",
            Value::str(concat!("v", env!("CARGO_PKG_VERSION"))),
        );
    }
    process
}

pub fn math_object() -> Value {
    let math = table_object(MATH);
    if let Value::Obj(o) = &math {
        obj_set(o, "PI", Value::Num(std::f64::consts::PI));
        obj_set(o, "E", Value::Num(std::f64::consts::E));
    }
    math
}

// ----- errors and conversions ------------------------------------------------

fn new_error(vm: &mut Vm, kind: &str, args: &[Value]) -> Value {
    let msg = match args.first() {
        Some(v) if !v.is_nullish() => to_str(v),
        _ => String::new(),
    };
    vm.make_error(kind, &msg)
}

fn error_ctor(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(new_error(vm, "Error", args))
}

fn type_error_ctor(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(new_error(vm, "TypeError", args))
}

fn range_error_ctor(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(new_error(vm, "RangeError", args))
}

fn syntax_error_ctor(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(new_error(vm, "SyntaxError", args))
}

fn reference_error_ctor(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(new_error(vm, "ReferenceError", args))
}

fn number(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Num(if args.is_empty() {
        0.0
    } else {
        to_num(&args[0])
    }))
}

fn string(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&args.first().map(to_str).unwrap_or_default()))
}

fn boolean(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(args.first().is_some_and(truthy)))
}

fn parse_int(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let text = to_str(&arg(args, 0));
    let t = text.trim_start();
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let requested = to_num(&arg(args, 1));
    let mut radix = if requested.is_nan() || requested == 0.0 {
        10
    } else {
        requested as u32
    };
    let mut digits = t;
    if (radix == 10 || radix == 16) && (t.starts_with("0x") || t.starts_with("0X")) {
        radix = 16;
        digits = &t[2..];
    }
    if !(2..=36).contains(&radix) {
        return Ok(Value::Num(f64::NAN));
    }
    let mut value = 0.0;
    let mut any = false;
    for c in digits.chars() {
        match c.to_digit(radix) {
            Some(d) => {
                value = value * f64::from(radix) + f64::from(d);
                any = true;
            }
            None => break,
        }
    }
    if !any {
        return Ok(Value::Num(f64::NAN));
    }
    Ok(Value::Num(if negative { -value } else { value }))
}

fn parse_float(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let text = to_str(&arg(args, 0));
    let t = text.trim_start();
    // Longest prefix that reads as a decimal number.
    let mut best = f64::NAN;
    for end in (1..=t.len().min(400)).filter(|&e| t.is_char_boundary(e)) {
        let prefix = &t[..end];
        let plausible = prefix
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
        if plausible {
            if let Ok(v) = prefix.parse::<f64>() {
                best = v;
            }
        }
    }
    Ok(Value::Num(best))
}

fn is_nan(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(to_num(&arg(args, 0)).is_nan()))
}

fn is_finite(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(to_num(&arg(args, 0)).is_finite()))
}

// ----- timers -----------------------------------------------------------------

fn schedule(vm: &mut Vm, args: &[Value], repeat: bool, immediate: bool) -> Result<Value, Value> {
    let callback = arg(args, 0);
    if !matches!(callback, Value::Func(_) | Value::Native(_)) {
        return Err(vm.make_error("TypeError", "The \"callback\" argument must be a function"));
    }
    let (delay_ms, rest_from) = if immediate {
        (0.0, 1)
    } else {
        let d = to_num(&arg(args, 1));
        (if d.is_nan() { 0.0 } else { d.max(0.0) }, 2)
    };
    let rest = args.get(rest_from..).unwrap_or(&[]).to_vec();
    let id = vm.add_timer(
        callback,
        Duration::from_millis(delay_ms as u64),
        rest,
        repeat,
    );
    Ok(Value::Num(id as f64))
}

fn set_timeout(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    schedule(vm, args, false, false)
}

fn set_interval(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    schedule(vm, args, true, false)
}

fn set_immediate(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    schedule(vm, args, false, true)
}

fn next_tick(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let callback = arg(args, 0);
    if !matches!(callback, Value::Func(_) | Value::Native(_)) {
        return Err(vm.make_error("TypeError", "The \"callback\" argument must be a function"));
    }
    vm.ticks
        .push_back((callback, args.get(1..).unwrap_or(&[]).to_vec()));
    Ok(Value::Undef)
}

fn clear_timer(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let id = to_num(&arg(args, 0));
    if id.is_finite() && id >= 0.0 {
        vm.clear_timer(id as u64);
    }
    Ok(Value::Undef)
}

// ----- namespaces -------------------------------------------------------------

/// Own keys of an object, or the indices of an array, as strings.
fn keys_of(v: &Value) -> Vec<String> {
    match v {
        Value::Obj(o) => {
            let props = o.borrow();
            let order = key_order(&props);
            order.into_iter().map(|i| props[i].0.to_string()).collect()
        }
        Value::Arr(a) => (0..a.borrow().len()).map(|i| i.to_string()).collect(),
        Value::Str(s) => (0..s.chars().count()).map(|i| i.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn value_of_key(v: &Value, key: &str) -> Value {
    match v {
        Value::Obj(o) => crate::value::obj_get(o, key).unwrap_or(Value::Undef),
        Value::Arr(a) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| a.borrow().get(i).cloned())
            .unwrap_or(Value::Undef),
        Value::Str(s) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| s.chars().nth(i))
            .map_or(Value::Undef, |c| Value::str(&c.to_string())),
        _ => Value::Undef,
    }
}

fn object_keys(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let keys = keys_of(&arg(args, 0));
    Ok(Value::arr(keys.iter().map(|k| Value::str(k)).collect()))
}

fn object_values(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let target = arg(args, 0);
    let values = keys_of(&target)
        .iter()
        .map(|k| value_of_key(&target, k))
        .collect();
    Ok(Value::arr(values))
}

fn object_entries(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let target = arg(args, 0);
    let pairs = keys_of(&target)
        .iter()
        .map(|k| Value::arr(vec![Value::str(k), value_of_key(&target, k)]))
        .collect();
    Ok(Value::arr(pairs))
}

fn is_array(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(matches!(arg(args, 0), Value::Arr(_))))
}

/// `Array.from(source, mapFn)`: the items of an array or string, or the first `length` properties of an
/// array-like object. A mapping function then replaces each item by its result.
fn array_from(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let items = match arg(args, 0) {
        Value::Arr(a) => a.borrow().clone(),
        Value::Str(s) => s.chars().map(crate::value::char_value).collect(),
        Value::Obj(o) => {
            let len = to_num(&crate::value::obj_get(&o, "length").unwrap_or(Value::Undef));
            let len = if len.is_nan() {
                0.0
            } else {
                len.trunc().clamp(0.0, crate::vm::MAX_ARRAY_GROW as f64)
            };
            (0..len as usize)
                .map(|i| crate::value::obj_get(&o, &i.to_string()).unwrap_or(Value::Undef))
                .collect()
        }
        _ => Vec::new(),
    };
    match arg(args, 1) {
        f @ (Value::Func(_) | Value::Native(_)) => {
            crate::builtins::array::map_items(vm, Value::arr(items), &f)
        }
        _ => Ok(Value::arr(items)),
    }
}

fn date_now(_vm: &mut Vm, _this: Value, _args: &[Value]) -> Result<Value, Value> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    Ok(Value::Num(ms))
}

fn process_exit(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let code = to_num(&arg(args, 0));
    vm.flush();
    std::process::exit(if code.is_nan() { 0 } else { code as i32 });
}

fn process_cwd(_vm: &mut Vm, _this: Value, _args: &[Value]) -> Result<Value, Value> {
    let dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(Value::str(&dir))
}

// ----- Math --------------------------------------------------------------------

/// `Math.round`: a half goes up, and a result of zero keeps the sign of its input, so `-0.4` gives `-0`.
fn round_half_up(x: f64) -> f64 {
    if !x.is_finite() || x == x.trunc() {
        return x;
    }
    let floor = x.floor();
    let r = if x - floor >= 0.5 { floor + 1.0 } else { floor };
    if r == 0.0 && x < 0.0 {
        -0.0
    } else {
        r
    }
}

macro_rules! unary_math {
    ($($name:ident => $f:expr),* $(,)?) => {
        $(
            fn $name(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Num(f(to_num(&arg(args, 0)))))
            }
        )*
    };
}

unary_math! {
    floor => f64::floor,
    ceil => f64::ceil,
    round => round_half_up,
    trunc => f64::trunc,
    abs => f64::abs,
    sqrt => f64::sqrt,
    sign => |x| if x == 0.0 || x.is_nan() { x } else { x.signum() },
    log => f64::ln,
    exp => f64::exp,
    sin => f64::sin,
    cos => f64::cos,
    tan => f64::tan,
}

fn pow(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Num(
        to_num(&arg(args, 0)).powf(to_num(&arg(args, 1))),
    ))
}

/// One step of `Math.min`, which orders -0 below +0 as JavaScript does.
fn min_step(best: f64, n: f64) -> f64 {
    if n < best || (n == 0.0 && best == 0.0 && n.is_sign_negative()) {
        n
    } else {
        best
    }
}

/// One step of `Math.max`, which orders +0 above -0.
fn max_step(best: f64, n: f64) -> f64 {
    if n > best || (n == 0.0 && best == 0.0 && n.is_sign_positive()) {
        n
    } else {
        best
    }
}

fn min(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let mut best = f64::INFINITY;
    for v in args {
        let n = to_num(v);
        if n.is_nan() {
            return Ok(Value::Num(f64::NAN));
        }
        best = min_step(best, n);
    }
    Ok(Value::Num(best))
}

fn max(_vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let mut best = f64::NEG_INFINITY;
    for v in args {
        let n = to_num(v);
        if n.is_nan() {
            return Ok(Value::Num(f64::NAN));
        }
        best = max_step(best, n);
    }
    Ok(Value::Num(best))
}

fn random(vm: &mut Vm, _this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Num(vm.random()))
}

// ----- require ----------------------------------------------------------------

/// Finds the file a relative `require` refers to: the path itself, `path.fly`, or `path/index.fly`.
fn resolve_file(p: &Path) -> Option<PathBuf> {
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    let mut with_ext = p.as_os_str().to_owned();
    with_ext.push(".fly");
    let with_ext = PathBuf::from(with_ext);
    if with_ext.is_file() {
        return Some(with_ext);
    }
    let index = p.join("index.fly");
    if index.is_file() {
        return Some(index);
    }
    None
}

fn require(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let name = to_str(&arg(args, 0));
    match name.as_str() {
        "fs" => return Ok(fs::fs_module()),
        "path" => return Ok(fs::path_module()),
        _ => {}
    }
    let relative = name.starts_with("./") || name.starts_with("../") || name.starts_with('/');
    if !relative {
        return Err(vm.make_error("Error", &format!("Cannot find module '{}'", name)));
    }
    let target = vm.current_dir().join(&name);
    let path = match resolve_file(&target) {
        Some(p) => p,
        None => return Err(vm.make_error("Error", &format!("Cannot find module '{}'", name))),
    };
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    vm.load_module(&path)
}
