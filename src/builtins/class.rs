//! Classes. The parser turns a `class` into one call of `%class`, which makes the constructor and its prototype, and
//! sets the methods and accessors on them (see `Parser::class_rest`).

use crate::builtins::arg;
use crate::value::{intern_name, new_obj, obj_put, to_str, truthy, NativeDef, Value};
use crate::vm::Vm;

pub static CLASS: NativeDef = NativeDef {
    name: "%class",
    func: make_class,
};

/// `%class(name, parent, constructor, members)`. The parent is undefined or a constructor, and each member is an array
/// of [kind, name, function, is_static], where the kind is "method", "get" or "set".
fn make_class(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let parent = arg(args, 1);
    let Value::Func(ctor) = arg(args, 2) else {
        return Err(vm.make_error("TypeError", "A class needs a constructor"));
    };
    let (parent_proto, parent_statics) = match &parent {
        Value::Func(p) => (Some(p.instance_proto()), Some(p.statics())),
        Value::Undef | Value::Null => (None, None),
        _ => {
            return Err(vm.make_error(
                "TypeError",
                "Class extends value is not a constructor or null",
            ))
        }
    };
    let prototype = new_obj(parent_proto);
    let statics = new_obj(parent_statics);
    obj_put(
        &prototype,
        &intern_name("constructor"),
        Value::Func(ctor.clone()),
    );
    {
        let mut info = ctor.info_mut();
        info.prototype = Some(prototype.clone());
        info.statics = Some(statics.clone());
        info.home = Some(prototype.clone());
        info.parent = match &parent {
            Value::Func(_) => Some(parent.clone()),
            _ => None,
        };
    }
    let members = match arg(args, 3) {
        Value::Arr(items) => items.borrow().clone(),
        _ => Vec::new(),
    };
    for member in members {
        let Value::Arr(parts) = member else {
            continue;
        };
        let parts = parts.borrow().clone();
        let [kind, name, func, is_static] = &parts[..] else {
            continue;
        };
        let Value::Func(f) = func else {
            continue;
        };
        let home = if truthy(is_static) {
            statics.clone()
        } else {
            prototype.clone()
        };
        f.info_mut().home = Some(home.clone());
        let key = intern_name(&to_str(name));
        match to_str(kind).as_str() {
            "get" => home
                .borrow_mut()
                .set_accessor(&key, false, Value::Func(f.clone())),
            "set" => home
                .borrow_mut()
                .set_accessor(&key, true, Value::Func(f.clone())),
            _ => obj_put(&home, &key, Value::Func(f.clone())),
        }
    }
    Ok(Value::Func(ctor))
}
