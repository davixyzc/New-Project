//! Map and Set. An object made by their constructors holds its entries (see `crate::collection`), and its methods are
//! found by name, as the methods of an array are. A `forEach` callback runs with no borrow of the entries held.

use crate::builtins::arg;
use crate::collection::{items_of, Collection};
use crate::value::{NativeDef, Props, Value};
use crate::vm::{Callback, Vm};

pub static MAP: NativeDef = NativeDef {
    name: "Map",
    func: map_new,
};

pub static SET: NativeDef = NativeDef {
    name: "Set",
    func: set_new,
};

/// `%items(value)`, which a destructuring pattern calls on its source (see `Parser::destructure`). An array or a
/// string is returned as it is, and a Map or a Set as the array of its items. Any other value throws.
pub static ITEMS: NativeDef = NativeDef {
    name: "%items",
    func: items,
};

const MAP_METHODS: &[NativeDef] = &[
    NativeDef {
        name: "clear",
        func: clear,
    },
    NativeDef {
        name: "delete",
        func: delete,
    },
    NativeDef {
        name: "entries",
        func: entries,
    },
    NativeDef {
        name: "forEach",
        func: for_each,
    },
    NativeDef {
        name: "get",
        func: get,
    },
    NativeDef {
        name: "has",
        func: has,
    },
    NativeDef {
        name: "keys",
        func: keys,
    },
    NativeDef {
        name: "set",
        func: set,
    },
    NativeDef {
        name: "values",
        func: values,
    },
];

const SET_METHODS: &[NativeDef] = &[
    NativeDef {
        name: "add",
        func: add,
    },
    NativeDef {
        name: "clear",
        func: clear,
    },
    NativeDef {
        name: "delete",
        func: delete,
    },
    NativeDef {
        name: "entries",
        func: entries,
    },
    NativeDef {
        name: "forEach",
        func: for_each,
    },
    NativeDef {
        name: "has",
        func: has,
    },
    NativeDef {
        name: "keys",
        func: keys,
    },
    NativeDef {
        name: "values",
        func: values,
    },
];

/// Whether `props` is a Map, for the constructor `Map`, or a Set, for `Set`. Any other pair is false.
pub fn is_instance(props: &Props, ctor: &NativeDef) -> bool {
    match props.collection.as_deref() {
        Some(c) if std::ptr::eq(ctor, &MAP) => !c.is_set,
        Some(c) if std::ptr::eq(ctor, &SET) => c.is_set,
        _ => false,
    }
}

/// The property `name` of an object that is a Map or a Set and has no own property of that name: its `size`, or one of
/// its methods.
pub fn property(props: &Props, name: &str) -> Option<Value> {
    let c = props.collection.as_deref()?;
    if name == "size" {
        return Some(Value::Num(c.len() as f64));
    }
    let table = if c.is_set { SET_METHODS } else { MAP_METHODS };
    table.iter().find(|d| d.name == name).map(Value::Native)
}

fn map_new(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    construct(vm, false, &arg(args, 0))
}

fn set_new(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    construct(vm, true, &arg(args, 0))
}

/// A new Map or Set, filled from `source` when there is one. A Map takes [key, value] entries, and a Set takes values.
fn construct(vm: &mut Vm, set: bool, source: &Value) -> Result<Value, Value> {
    let mut c = Collection::new(set);
    if matches!(source, Value::Undef | Value::Null) {
        return Ok(Value::collection(c));
    }
    let Some(items) = items_of(source) else {
        return Err(vm.make_error("TypeError", "value is not iterable"));
    };
    for item in items {
        if set {
            c.set(item.clone(), item);
            continue;
        }
        let Value::Arr(pair) = &item else {
            return Err(vm.make_error("TypeError", "Iterator value is not an entry object"));
        };
        let (key, value) = {
            let pair = pair.borrow();
            (
                pair.first().cloned().unwrap_or(Value::Undef),
                pair.get(1).cloned().unwrap_or(Value::Undef),
            )
        };
        c.set(key, value);
    }
    Ok(Value::collection(c))
}

fn items(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let v = arg(args, 0);
    match &v {
        Value::Arr(_) | Value::Str(_) => Ok(v.clone()),
        _ => match items_of(&v) {
            Some(items) => Ok(Value::arr(items)),
            None => Err(vm.make_error("TypeError", "value is not iterable")),
        },
    }
}

/// Runs `f` on the entries of the Map or Set that `this` is.
fn with<R>(vm: &mut Vm, this: &Value, f: impl FnOnce(&mut Collection) -> R) -> Result<R, Value> {
    if let Value::Obj(o) = this {
        if let Some(c) = o.borrow_mut().collection.as_deref_mut() {
            return Ok(f(c));
        }
    }
    Err(vm.make_error(
        "TypeError",
        "Method called on an object that is not a Map or a Set",
    ))
}

fn get(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let key = arg(args, 0);
    with(vm, &this, |c| c.get(&key).cloned().unwrap_or(Value::Undef))
}

fn set(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let (key, value) = (arg(args, 0), arg(args, 1));
    with(vm, &this, |c| c.set(key, value))?;
    Ok(this)
}

fn add(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let value = arg(args, 0);
    with(vm, &this, |c| c.set(value.clone(), value))?;
    Ok(this)
}

fn has(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let key = arg(args, 0);
    with(vm, &this, |c| Value::Bool(c.has(&key)))
}

fn delete(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let key = arg(args, 0);
    with(vm, &this, |c| Value::Bool(c.delete(&key)))
}

fn clear(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    with(vm, &this, |c| c.clear())?;
    Ok(Value::Undef)
}

/// The keys, as an array. For a Set these are its values.
fn keys(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let items = with(vm, &this, |c| {
        c.pairs()
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<Value>>()
    })?;
    Ok(Value::arr(items))
}

/// The values, as an array.
fn values(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let items = with(vm, &this, |c| {
        c.pairs()
            .into_iter()
            .map(|(_, v)| v)
            .collect::<Vec<Value>>()
    })?;
    Ok(Value::arr(items))
}

/// The [key, value] entries, as an array of two-item arrays.
fn entries(vm: &mut Vm, this: Value, _args: &[Value]) -> Result<Value, Value> {
    let items = with(vm, &this, |c| {
        c.pairs()
            .into_iter()
            .map(|(k, v)| Value::arr(vec![k, v]))
            .collect::<Vec<Value>>()
    })?;
    Ok(Value::arr(items))
}

/// Calls the callback with each entry, in insertion order, as `(value, key, collection)`. The keys are taken first, so
/// an entry added by the callback is not visited. An entry that the callback deletes before its turn is skipped, and
/// one that it changes before its turn is passed with its new value.
fn for_each(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, Value> {
    let keys = with(vm, &this, |c| {
        c.pairs()
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<Value>>()
    })?;
    let mut cb = Callback::new(vm, &arg(args, 0), arg(args, 1));
    for key in keys {
        if let Some(value) = with(vm, &this, |c| c.get(&key).cloned())? {
            cb.call(vm, [value, key, this.clone()])?;
        }
    }
    cb.finish(vm);
    Ok(Value::Undef)
}
