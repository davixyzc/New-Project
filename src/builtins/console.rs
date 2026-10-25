//! `console.*` and the value formatting used by `console.log`.

use crate::collection::Collection;
use crate::value::{class_name, num_to_string, obj_get, NativeDef, Value};
use crate::vm::Vm;

pub static METHODS: &[NativeDef] = &[
    NativeDef {
        name: "log",
        func: log,
    },
    NativeDef {
        name: "info",
        func: log,
    },
    NativeDef {
        name: "debug",
        func: log,
    },
    NativeDef {
        name: "error",
        func: error,
    },
    NativeDef {
        name: "warn",
        func: error,
    },
];

/// How deep nested arrays and objects are printed before they are abbreviated.
const MAX_DEPTH: usize = 2;

fn render(args: &[Value]) -> String {
    args.iter()
        .map(|a| inspect(a, true, 0))
        .collect::<Vec<_>>()
        .join(" ")
}

fn log(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    let line = render(args);
    vm.write_out(&line);
    vm.write_out("\n");
    Ok(Value::Undef)
}

fn error(vm: &mut Vm, _this: Value, args: &[Value]) -> Result<Value, Value> {
    vm.flush();
    eprintln!("{}", render(args));
    Ok(Value::Undef)
}

/// Formats a value for display. `top` is true for values printed directly, where strings are unquoted.
pub fn inspect(v: &Value, top: bool, depth: usize) -> String {
    match v {
        Value::Undef => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_string(*n),
        Value::Str(s) => {
            if top {
                s.to_string()
            } else {
                quote(s)
            }
        }
        Value::Arr(a) => {
            if depth > MAX_DEPTH {
                return "[Array]".to_string();
            }
            let items = a.borrow();
            if items.is_empty() {
                return "[]".to_string();
            }
            let parts: Vec<String> = items.iter().map(|x| inspect(x, false, depth + 1)).collect();
            format!("[ {} ]", parts.join(", "))
        }
        Value::Obj(o) => {
            // Error objects print their stack, as Node.js does.
            if let Some(Value::Str(stack)) = obj_get(o, "stack") {
                return stack.to_string();
            }
            if depth > MAX_DEPTH {
                return "[Object]".to_string();
            }
            let props = o.borrow();
            if let Some(c) = &props.collection {
                return collection_text(c, depth);
            }
            // An instance is printed with the name of its class, as Node.js prints it.
            let prefix = class_name(&props).map_or(String::new(), |n| format!("{} ", n));
            if props.is_empty() {
                return format!("{}{{}}", prefix);
            }
            let parts: Vec<String> = props
                .iter()
                .map(|(k, val)| format!("{}: {}", key_text(k), inspect(val, false, depth + 1)))
                .collect();
            format!("{}{{ {} }}", prefix, parts.join(", "))
        }
        Value::Func(c) => {
            if c.proto.name.is_empty() {
                "[Function (anonymous)]".to_string()
            } else {
                format!("[Function: {}]", c.proto.name)
            }
        }
        Value::Native(d) => format!("[Function: {}]", d.name),
        Value::Cell(c) => inspect(&c.borrow(), top, depth),
    }
}

/// A Map or a Set, printed as Node.js prints it: `Map(2) { 'a' => 1, 'b' => 2 }`.
fn collection_text(c: &Collection, depth: usize) -> String {
    let name = if c.is_set { "Set" } else { "Map" };
    let parts: Vec<String> = c
        .pairs()
        .iter()
        .map(|(k, v)| {
            if c.is_set {
                inspect(k, false, depth + 1)
            } else {
                format!(
                    "{} => {}",
                    inspect(k, false, depth + 1),
                    inspect(v, false, depth + 1)
                )
            }
        })
        .collect();
    if parts.is_empty() {
        format!("{}({}) {{}}", name, c.len())
    } else {
        format!("{}({}) {{ {} }}", name, c.len(), parts.join(", "))
    }
}

fn quote(s: &str) -> String {
    format!(
        "'{}'",
        s.replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
    )
}

/// Object keys print bare when they are valid identifiers, quoted otherwise.
fn key_text(k: &str) -> String {
    let mut chars = k.chars();
    let bare = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if bare {
        k.to_string()
    } else {
        quote(k)
    }
}
