//! Array methods. Callbacks run through `Vm::call`, and no `RefCell` borrow is held across a call.

use std::cmp::Ordering;

use crate::builtins::arg;
use crate::value::{strict_eq, to_num, to_str, truthy, ArrRef, NativeDef, Value};
use crate::vm::Vm;

pub static METHODS: &[NativeDef] = &[
    NativeDef {
        name: "push",
        func: push,
    },
    NativeDef {
        name: "pop",
        func: pop,
    },
    NativeDef {
        name: "shift",
        func: shift,
    },
    NativeDef {
        name: "unshift",
        func: unshift,
    },
    NativeDef {
        name: "slice",
        func: slice,
    },
    NativeDef {
        name: "indexOf",
        func: index_of,
    },
    NativeDef {
        name: "includes",
        func: includes,
    },
    NativeDef {
        name: "join",
        func: join,
    },
    NativeDef {
        name: "concat",
        func: concat,
    },
    NativeDef {
        name: "reverse",
        func: reverse,
    },
    NativeDef {
        name: "map",
        func: map,
    },
    NativeDef {
        name: "filter",
        func: filter,
    },
    NativeDef {
        name: "forEach",
        func: for_each,
    },
    NativeDef {
        name: "reduce",
        func: reduce,
    },
    NativeDef {
        name: "find",
        func: find,
    },
    NativeDef {
        name: "findIndex",
        func: find_index,
    },
    NativeDef {
        name: "some",
        func: some,
    },
    NativeDef {
        name: "every",
        func: every,
    },
    NativeDef {
        name: "sort",
        func: sort,
    },
];

pub fn method(name: &str) -> Option<&'static NativeDef> {
    METHODS.iter().find(|d| d.name == name)
}

fn array_of(vm: &mut Vm, this: &Value) -> Result<ArrRef, Value> {
    match this {
        Value::Arr(a) => Ok(a.clone()),
        _ => Err(vm.make_error("TypeError", "Array method called on a non-array")),
    }
}

/// The element at `i`, copied out so that no borrow is held while a callback runs.
fn element(a: &ArrRef, i: usize) -> Option<Value> {
    a.borrow().get(i).cloned()
}

/// Turns optional start/end arguments into a valid range. Negative values count from the end.
pub fn bounds(len: usize, start: &Value, end: &Value) -> (usize, usize) {
    let clamp = |v: f64| -> usize {
        if v.is_nan() {
            0
        } else if v < 0.0 {
            (len as f64 + v).max(0.0) as usize
        } else {
            (v as usize).min(len)
        }
    };
    let s = if start.is_nullish() {
        0
    } else {
        clamp(to_num(start))
    };
    let e = if end.is_nullish() {
        len
    } else {
        clamp(to_num(end))
    };
    (s, e.max(s))
}

fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) if x.is_nan() && y.is_nan() => true,
        _ => strict_eq(a, b),
    }
}

fn push(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut items = a.borrow_mut();
    match args {
        [v] => items.push(v.clone()),
        _ => items.extend_from_slice(args),
    }
    Ok(Value::Num(items.len() as f64))
}

fn pop(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let v = a.borrow_mut().pop();
    Ok(v.unwrap_or(Value::Undef))
}

fn shift(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut items = a.borrow_mut();
    Ok(if items.is_empty() {
        Value::Undef
    } else {
        items.remove(0)
    })
}

fn unshift(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut items = a.borrow_mut();
    for (i, v) in args.iter().enumerate() {
        items.insert(i, v.clone());
    }
    Ok(Value::Num(items.len() as f64))
}

fn slice(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let items = a.borrow();
    let (s, e) = bounds(items.len(), &arg(args, 0), &arg(args, 1));
    Ok(Value::arr(items[s..e].to_vec()))
}

fn index_of(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let needle = arg(args, 0);
    let pos = a.borrow().iter().position(|x| strict_eq(x, &needle));
    Ok(Value::Num(pos.map_or(-1.0, |p| p as f64)))
}

fn includes(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let needle = arg(args, 0);
    let found = a.borrow().iter().any(|x| same_value_zero(x, &needle));
    Ok(Value::Bool(found))
}

fn join(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let sep = match arg(args, 0) {
        Value::Undef => ",".to_string(),
        v => to_str(&v),
    };
    let text = a
        .borrow()
        .iter()
        .map(|x| {
            if x.is_nullish() {
                String::new()
            } else {
                to_str(x)
            }
        })
        .collect::<Vec<_>>()
        .join(&sep);
    Ok(Value::str(&text))
}

fn concat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut out = a.borrow().clone();
    for v in args {
        match v {
            Value::Arr(b) => out.extend(b.borrow().iter().cloned()),
            other => out.push(other.clone()),
        }
    }
    Ok(Value::arr(out))
}

fn reverse(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    a.borrow_mut().reverse();
    Ok(this)
}

fn map(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let this_arg = arg(args, 1);
    let len = a.borrow().len();
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        out.push(vm.call_moved(
            &f,
            this_arg.clone(),
            [item, Value::Num(i as f64), this.clone()],
        )?);
    }
    Ok(Value::arr(out))
}

fn filter(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let this_arg = arg(args, 1);
    let len = a.borrow().len();
    let mut out = Vec::new();
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let keep = vm.call_moved(
            &f,
            this_arg.clone(),
            [item.clone(), Value::Num(i as f64), this.clone()],
        )?;
        if truthy(&keep) {
            out.push(item);
        }
    }
    Ok(Value::arr(out))
}

fn for_each(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let this_arg = arg(args, 1);
    let len = a.borrow().len();
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        vm.call_moved(
            &f,
            this_arg.clone(),
            [item, Value::Num(i as f64), this.clone()],
        )?;
    }
    Ok(Value::Undef)
}

fn reduce(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let len = a.borrow().len();
    let (mut acc, start) = if args.len() >= 2 {
        (args[1].clone(), 0)
    } else {
        match element(&a, 0) {
            Some(v) => (v, 1),
            None => {
                return Err(
                    vm.make_error("TypeError", "Reduce of empty array with no initial value")
                )
            }
        }
    };
    for i in start..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        acc = vm.call_moved(
            &f,
            Value::Undef,
            [acc, item, Value::Num(i as f64), this.clone()],
        )?;
    }
    Ok(acc)
}

/// Shared driver for find, findIndex, some and every: stops at the first element where the callback decides.
fn search(
    vm: &mut Vm,
    this: &Value,
    f: &Value,
    this_arg: &Value,
    stop_when: bool,
) -> Result<Option<(usize, Value)>, Value> {
    let a = match this {
        Value::Arr(a) => a.clone(),
        _ => return Err(vm.make_error("TypeError", "Array method called on a non-array")),
    };
    let len = a.borrow().len();
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let r = vm.call_moved(
            f,
            this_arg.clone(),
            [item.clone(), Value::Num(i as f64), this.clone()],
        )?;
        if truthy(&r) == stop_when {
            return Ok(Some((i, item)));
        }
    }
    Ok(None)
}

fn find(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(search(vm, &this, &arg(args, 0), &arg(args, 1), true)?.map_or(Value::Undef, |(_, v)| v))
}

fn find_index(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Num(
        search(vm, &this, &arg(args, 0), &arg(args, 1), true)?.map_or(-1.0, |(i, _)| i as f64),
    ))
}

fn some(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(
        search(vm, &this, &arg(args, 0), &arg(args, 1), true)?.is_some(),
    ))
}

fn every(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    Ok(Value::Bool(
        search(vm, &this, &arg(args, 0), &arg(args, 1), false)?.is_none(),
    ))
}

/// Stable merge sort with a fallible comparator. Returns the number that orders `a` relative to `b`.
fn merge_sort(
    items: Vec<Value>,
    cmp: &mut dyn FnMut(&Value, &Value) -> Result<f64, Value>,
) -> Result<Vec<Value>, Value> {
    if items.len() <= 1 {
        return Ok(items);
    }
    let mut left = items;
    let right = left.split_off(left.len() / 2);
    let left = merge_sort(left, cmp)?;
    let right = merge_sort(right, cmp)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if cmp(&left[i], &right[j])? <= 0.0 {
            out.push(left[i].clone());
            i += 1;
        } else {
            out.push(right[j].clone());
            j += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    Ok(out)
}

fn sort(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let compare_fn = arg(args, 0);
    let items = a.borrow().clone();
    let sorted = if compare_fn.is_nullish() {
        merge_sort(items, &mut |x: &Value, y: &Value| {
            Ok(match to_str(x).cmp(&to_str(y)) {
                Ordering::Less => -1.0,
                Ordering::Equal => 0.0,
                Ordering::Greater => 1.0,
            })
        })?
    } else {
        merge_sort(items, &mut |x: &Value, y: &Value| {
            let r = vm.call_moved(&compare_fn, Value::Undef, [x.clone(), y.clone()])?;
            Ok(to_num(&r))
        })?
    };
    *a.borrow_mut() = sorted;
    Ok(this)
}
