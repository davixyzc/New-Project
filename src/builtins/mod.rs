//! Built-in globals and modules. Each submodule owns a static table of native functions,
//! so a built-in method can be referenced from a `Value` without allocating.

pub mod array;
pub mod class;
pub mod collections;
pub mod console;
pub mod fs;
pub mod global;
pub mod json;
pub mod string;

use crate::value::{make_obj, NativeDef, Value};
use crate::vm::Vm;

pub fn install(vm: &mut Vm) {
    vm.set_global("console", table_object(console::METHODS));
    vm.set_global("process", global::process_object(vm.argv.clone()));
    vm.set_global("Math", global::math_object());
    vm.set_global("Object", table_object(global::OBJECT));
    vm.set_global("Array", table_object(global::ARRAY));
    vm.set_global("Map", Value::Native(&collections::MAP));
    vm.set_global("Set", Value::Native(&collections::SET));
    vm.set_global("%items", Value::Native(&collections::ITEMS));
    vm.set_global("%rest", Value::Native(&global::REST));
    vm.set_global("%class", Value::Native(&class::CLASS));
    vm.set_global("Date", table_object(global::DATE));
    vm.set_global("JSON", json::json_object());
    for def in global::FUNCTIONS {
        vm.set_global(def.name, Value::Native(def));
    }
    vm.set_global("require", Value::Native(&global::REQUIRE));
    vm.set_global("NaN", Value::Num(f64::NAN));
    vm.set_global("Infinity", Value::Num(f64::INFINITY));
    // `globalThis` holds the built-in globals. Its properties are copies, so assigning to it does not change a global.
    let mut props = crate::value::Props::default();
    for (name, value) in vm.globals.iter() {
        if !name.starts_with('%') {
            props.push(name.clone(), value.clone());
        }
    }
    vm.set_global(
        "globalThis",
        Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(props))),
    );
}

/// The definition called `name` in a table of built-in methods. `index` holds the table sorted by name. It is built
/// the first time the table is searched, so a lookup is a binary search rather than a scan of every method.
pub fn sorted_lookup(
    table: &'static [NativeDef],
    index: &'static std::sync::OnceLock<Vec<&'static NativeDef>>,
    name: &str,
) -> Option<&'static NativeDef> {
    let sorted = index.get_or_init(|| {
        let mut defs: Vec<&'static NativeDef> = table.iter().collect();
        defs.sort_by_key(|d| d.name);
        defs
    });
    sorted
        .binary_search_by_key(&name, |d| d.name)
        .ok()
        .map(|i| sorted[i])
}

/// An object whose properties are the functions of a static table.
pub fn table_object(table: &'static [NativeDef]) -> Value {
    make_obj(table.iter().map(|d| (d.name, Value::Native(d))).collect())
}

/// The i-th argument, or `undefined` when it was not passed.
pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undef)
}
