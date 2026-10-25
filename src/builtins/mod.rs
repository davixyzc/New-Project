//! Built-in globals and modules. Each submodule owns a static table of native functions,
//! so a built-in method can be referenced from a `Value` without allocating.

pub mod array;
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
    vm.set_global("Date", table_object(global::DATE));
    vm.set_global("JSON", json::json_object());
    for def in global::FUNCTIONS {
        vm.set_global(def.name, Value::Native(def));
    }
    vm.set_global("require", Value::Native(&global::REQUIRE));
    vm.set_global("NaN", Value::Num(f64::NAN));
    vm.set_global("Infinity", Value::Num(f64::INFINITY));
}

/// An object whose properties are the functions of a static table.
pub fn table_object(table: &'static [NativeDef]) -> Value {
    make_obj(table.iter().map(|d| (d.name, Value::Native(d))).collect())
}

/// The i-th argument, or `undefined` when it was not passed.
pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undef)
}
