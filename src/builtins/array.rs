//! Array methods. Callbacks run through `Vm::call`, and no `RefCell` borrow is held across a call.

use std::cmp::Ordering;

use crate::builtins::arg;
use crate::value::{join_items, strict_eq, to_num, to_str, truthy, ArrRef, NativeDef, Value};
use crate::vm::{Callback, Vm, MAX_ARRAY_GROW};

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
    NativeDef {
        name: "lastIndexOf",
        func: last_index_of,
    },
    NativeDef {
        name: "toString",
        func: to_string_method,
    },
    NativeDef {
        name: "splice",
        func: splice,
    },
    NativeDef {
        name: "flat",
        func: flat,
    },
    NativeDef {
        name: "reduceRight",
        func: reduce_right,
    },
    NativeDef {
        name: "at",
        func: at,
    },
    NativeDef {
        name: "fill",
        func: fill,
    },
];

/// The `push` method. It is the first entry of `METHODS`, and the VM runs it in place (see
/// `Vm::push_in_place`).
pub fn push_def() -> &'static NativeDef {
    debug_assert_eq!(METHODS[0].name, "push");
    &METHODS[0]
}

static METHOD_INDEX: std::sync::OnceLock<Vec<&'static NativeDef>> = std::sync::OnceLock::new();

pub fn method(name: &str) -> Option<&'static NativeDef> {
    crate::builtins::sorted_lookup(METHODS, &METHOD_INDEX, name)
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

/// Turns start/end arguments into a valid range. Negative values count from the end, and fractions drop toward
/// zero first. Only a missing `end` means the end of the sequence; `null` is zero.
pub fn bounds(len: usize, start: &Value, end: &Value) -> (usize, usize) {
    let clamp = |v: f64| -> usize {
        let v = if v.is_nan() { 0.0 } else { v.trunc() };
        if v < 0.0 {
            (len as f64 + v).max(0.0) as usize
        } else {
            (v as usize).min(len)
        }
    };
    let s = clamp(to_num(start));
    let e = match end {
        Value::Undef => len,
        _ => clamp(to_num(end)),
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
    let Value::Arr(a) = &this else {
        return Err(vm.make_error("TypeError", "Array method called on a non-array"));
    };
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
    Ok(Value::str(&join_items(&a, &sep, None)))
}

fn last_index_of(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let items = a.borrow();
    let target = arg(args, 0);
    let len = items.len() as f64;
    // Without a position the search starts at the last item. A position that is given counts as a number,
    // so `undefined` there means 0.
    let from = if args.len() < 2 {
        len - 1.0
    } else {
        let n = to_num(&args[1]);
        if n.is_nan() {
            0.0
        } else {
            n.trunc()
        }
    };
    let start = if from >= 0.0 {
        from.min(len - 1.0)
    } else {
        len + from
    };
    if start < 0.0 {
        return Ok(Value::Num(-1.0));
    }
    let found = (0..=start as usize)
        .rev()
        .find(|&i| strict_eq(&items[i], &target));
    Ok(Value::Num(found.map_or(-1.0, |i| i as f64)))
}

/// `splice(start, deleteCount, ...items)`: removes items from the array and inserts `items` in their place. The
/// removed items are returned. With no `deleteCount`, everything from `start` on is removed.
fn splice(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let len = a.borrow().len();
    let (start, _) = bounds(len, &arg(args, 0), &Value::Undef);
    let count = match args.len() {
        0 => 0,
        1 => len - start,
        _ => {
            let n = to_num(&args[1]);
            let n = if n.is_nan() { 0.0 } else { n.trunc() };
            n.clamp(0.0, (len - start) as f64) as usize
        }
    };
    let items = args.iter().skip(2).cloned();
    let removed: Vec<Value> = a.borrow_mut().splice(start..start + count, items).collect();
    Ok(Value::arr(removed))
}

/// `flat(depth)`: the items with nested arrays flattened `depth` levels down (one by default).
fn flat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let depth = match arg(args, 0) {
        Value::Undef => 1.0,
        v => to_num(&v).trunc(),
    };
    let mut out = Vec::new();
    flatten_into(&a.borrow(), depth, &mut out);
    Ok(Value::arr(out))
}

fn flatten_into(items: &[Value], depth: f64, out: &mut Vec<Value>) {
    for item in items {
        match item {
            Value::Arr(inner) if depth >= 1.0 => flatten_into(&inner.borrow(), depth - 1.0, out),
            other => out.push(other.clone()),
        }
    }
}

/// `reduceRight(f, initial)`: `reduce` from the end of the array.
fn reduce_right(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let mut cb = Callback::new(vm, &f, Value::Undef);
    let len = a.borrow().len();
    let (mut acc, end) = if args.len() >= 2 {
        (args[1].clone(), len)
    } else if len == 0 {
        cb.finish(vm);
        return Err(vm.make_error("TypeError", "Reduce of empty array with no initial value"));
    } else {
        (element(&a, len - 1).unwrap_or(Value::Undef), len - 1)
    };
    for i in (0..end).rev() {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        acc = cb.call(vm, [acc, item, Value::Num(i as f64), this.clone()])?;
    }
    cb.finish(vm);
    Ok(acc)
}

/// `at(index)`: the item at `index`, counting a negative index from the end. Outside the array is undefined.
fn at(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let len = a.borrow().len() as f64;
    let n = to_num(&arg(args, 0));
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let i = if n < 0.0 { len + n } else { n };
    if i < 0.0 || i >= len {
        return Ok(Value::Undef);
    }
    Ok(element(&a, i as usize).unwrap_or(Value::Undef))
}

/// `fill(value, start, end)`: sets the items in the range to `value`, and returns the array.
fn fill(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let len = a.borrow().len();
    let (start, end) = bounds(len, &arg(args, 1), &arg(args, 2));
    let value = arg(args, 0);
    for slot in &mut a.borrow_mut()[start..end] {
        *slot = value.clone();
    }
    Ok(this)
}

/// `Array.prototype.toString`: the items joined with commas, as `String(array)` gives.
fn to_string_method(_vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    Ok(Value::str(&to_str(&this)))
}

fn concat(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    // The size of the result is known before anything is copied, so a huge concat fails without allocating.
    let total = a.borrow().len()
        + args
            .iter()
            .map(|v| match v {
                Value::Arr(b) => b.borrow().len(),
                _ => 1,
            })
            .sum::<usize>();
    if total > MAX_ARRAY_GROW {
        return Err(vm.make_error("RangeError", "Invalid array length"));
    }
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

/// `map` of an array held in `items`, with `f` as the callback. Used by `Array.from` for its mapping function.
pub fn map_items(vm: &mut Vm, items: Value, f: &Value) -> Result<Value, Value> {
    map(vm, items, &[f.clone()])
}

fn map(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut cb = Callback::new(vm, &arg(args, 0), arg(args, 1));
    let len = a.borrow().len();
    let mut out = Vec::with_capacity(len);
    let mut start = 0;
    // While the elements are numbers and the callback runs on numbers, no interpreter code runs, so the
    // array can stay borrowed.
    if let Some(entry) = cb.numeric_entry() {
        start += entry.map(&a.borrow(), start, &mut out);
    }
    for i in start..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let third = if cb.arity() > 2 {
            this.clone()
        } else {
            Value::Undef
        };
        out.push(cb.call(vm, [item, Value::Num(i as f64), third])?);
    }
    cb.finish(vm);
    Ok(Value::arr(out))
}

fn filter(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut cb = Callback::new(vm, &arg(args, 0), arg(args, 1));
    let len = a.borrow().len();
    let mut out = Vec::new();
    let mut start = 0;
    if let Some(entry) = cb.numeric_entry() {
        start += entry.filter(&a.borrow(), start, &mut out);
    }
    for i in start..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let third = if cb.arity() > 2 {
            this.clone()
        } else {
            Value::Undef
        };
        let keep = cb.call(vm, [item.clone(), Value::Num(i as f64), third])?;
        if truthy(&keep) {
            out.push(item);
        }
    }
    cb.finish(vm);
    Ok(Value::arr(out))
}

fn for_each(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let mut cb = Callback::new(vm, &arg(args, 0), arg(args, 1));
    let len = a.borrow().len();
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let third = if cb.arity() > 2 {
            this.clone()
        } else {
            Value::Undef
        };
        cb.call(vm, [item, Value::Num(i as f64), third])?;
    }
    cb.finish(vm);
    Ok(Value::Undef)
}

fn reduce(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let a = array_of(vm, &this)?;
    let f = arg(args, 0);
    let mut cb = Callback::new(vm, &f, Value::Undef);
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
    let mut i = start;
    if let Some(entry) = cb.numeric_entry() {
        if let Value::Num(first) = acc {
            let (value, done) = entry.reduce(&a.borrow(), i, first);
            acc = Value::Num(value);
            i += done;
        }
    }
    for i in i..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let last = if cb.arity() > 3 {
            this.clone()
        } else {
            Value::Undef
        };
        acc = cb.call(vm, [acc, item, Value::Num(i as f64), last])?;
    }
    cb.finish(vm);
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
    let mut cb = Callback::new(vm, f, this_arg.clone());
    let len = a.borrow().len();
    for i in 0..len {
        let item = match element(&a, i) {
            Some(v) => v,
            None => break,
        };
        let third = if cb.arity() > 2 {
            this.clone()
        } else {
            Value::Undef
        };
        let r = cb.call(vm, [item.clone(), Value::Num(i as f64), third])?;
        if truthy(&r) == stop_when {
            cb.finish(vm);
            return Ok(Some((i, item)));
        }
    }
    cb.finish(vm);
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
        let mut cb = Callback::new(vm, &compare_fn, Value::Undef);
        let sorted = merge_sort(items, &mut |x: &Value, y: &Value| {
            let r = cb.call(vm, [x.clone(), y.clone()])?;
            Ok(to_num(&r))
        });
        cb.finish(vm);
        sorted?
    };
    *a.borrow_mut() = sorted;
    Ok(this)
}
