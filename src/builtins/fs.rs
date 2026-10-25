//! The `fs` and `path` modules. File I/O is synchronous; the callback forms deliver
//! their result from the event loop, like Node.js does.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::builtins::{arg, table_object};
use crate::value::{obj_set, to_str, NativeDef, Value};
use crate::vm::Vm;

pub static FS: &[NativeDef] = &[
    NativeDef {
        name: "readFileSync",
        func: read_file_sync,
    },
    NativeDef {
        name: "writeFileSync",
        func: write_file_sync,
    },
    NativeDef {
        name: "appendFileSync",
        func: append_file_sync,
    },
    NativeDef {
        name: "existsSync",
        func: exists_sync,
    },
    NativeDef {
        name: "readdirSync",
        func: readdir_sync,
    },
    NativeDef {
        name: "mkdirSync",
        func: mkdir_sync,
    },
    NativeDef {
        name: "unlinkSync",
        func: unlink_sync,
    },
    NativeDef {
        name: "readFile",
        func: read_file,
    },
    NativeDef {
        name: "writeFile",
        func: write_file,
    },
];

pub static PATH: &[NativeDef] = &[
    NativeDef {
        name: "join",
        func: join,
    },
    NativeDef {
        name: "resolve",
        func: resolve,
    },
    NativeDef {
        name: "basename",
        func: basename,
    },
    NativeDef {
        name: "dirname",
        func: dirname,
    },
    NativeDef {
        name: "extname",
        func: extname,
    },
];

pub fn fs_module() -> Value {
    table_object(FS)
}

pub fn path_module() -> Value {
    let path = table_object(PATH);
    if let Value::Obj(o) = &path {
        obj_set(o, "sep", Value::str("/"));
    }
    path
}

/// Converts an I/O error into a Node.js-style error object, e.g. `ENOENT: no such file or directory, open 'x'`.
fn io_error(vm: &mut Vm, e: std::io::Error, syscall: &str, path: &str) -> Value {
    let (code, desc) = match e.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        _ => ("EIO", "i/o error"),
    };
    vm.make_error(
        "Error",
        &format!("{}: {}, {} '{}'", code, desc, syscall, path),
    )
}

fn path_arg(args: &[Value]) -> String {
    to_str(&arg(args, 0))
}

fn read_file_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    match std::fs::read_to_string(&p) {
        Ok(text) => Ok(Value::str(&text)),
        Err(e) => Err(io_error(vm, e, "open", &p)),
    }
}

fn write_file_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    match std::fs::write(&p, to_str(&arg(&args, 1))) {
        Ok(()) => Ok(Value::Undef),
        Err(e) => Err(io_error(vm, e, "open", &p)),
    }
}

fn append_file_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .and_then(|mut f| f.write_all(to_str(&arg(&args, 1)).as_bytes()));
    match result {
        Ok(()) => Ok(Value::Undef),
        Err(e) => Err(io_error(vm, e, "open", &p)),
    }
}

fn exists_sync(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    Ok(Value::Bool(Path::new(&path_arg(&args)).exists()))
}

fn readdir_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let entries = match std::fs::read_dir(&p) {
        Ok(entries) => entries,
        Err(e) => return Err(io_error(vm, e, "scandir", &p)),
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(Value::arr(names.iter().map(|n| Value::str(n)).collect()))
}

fn mkdir_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    match std::fs::create_dir_all(&p) {
        Ok(()) => Ok(Value::Undef),
        Err(e) => Err(io_error(vm, e, "mkdir", &p)),
    }
}

fn unlink_sync(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(Value::Undef),
        Err(e) => Err(io_error(vm, e, "unlink", &p)),
    }
}

/// Queues `callback(err, data)` on the event loop.
fn deliver(vm: &mut Vm, callback: Value, err: Value, data: Value) {
    vm.add_timer(callback, Duration::ZERO, vec![err, data], false);
}

fn read_file(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let callback = args.last().cloned().unwrap_or(Value::Undef);
    match std::fs::read_to_string(&p) {
        Ok(text) => deliver(vm, callback, Value::Null, Value::str(&text)),
        Err(e) => {
            let err = io_error(vm, e, "open", &p);
            deliver(vm, callback, err, Value::Undef);
        }
    }
    Ok(Value::Undef)
}

fn write_file(vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let callback = args.last().cloned().unwrap_or(Value::Undef);
    match std::fs::write(&p, to_str(&arg(&args, 1))) {
        Ok(()) => deliver(vm, callback, Value::Null, Value::Undef),
        Err(e) => {
            let err = io_error(vm, e, "open", &p);
            deliver(vm, callback, err, Value::Undef);
        }
    }
    Ok(Value::Undef)
}

// ----- path -------------------------------------------------------------------

/// Joins segments and resolves `.` and `..` lexically.
fn normalize(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(last) if *last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{}", joined),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

fn join(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let parts: Vec<String> = args.iter().map(to_str).filter(|s| !s.is_empty()).collect();
    Ok(Value::str(&normalize(&parts.join("/"))))
}

fn resolve(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    // Later absolute segments reset the path; relative ones are appended with a separator.
    let mut acc = String::new();
    for part in args.iter().map(to_str) {
        if part.starts_with('/') || acc.is_empty() {
            acc = part;
        } else {
            acc = format!("{}/{}", acc, part);
        }
    }
    if !acc.starts_with('/') {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        acc = format!("{}/{}", cwd, acc);
    }
    Ok(Value::str(&normalize(&acc)))
}

fn basename(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let name = Path::new(&p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = match arg(&args, 1) {
        Value::Undef => None,
        v => Some(to_str(&v)),
    };
    let name = match ext {
        Some(ext) => name
            .strip_suffix(ext.as_str())
            .map(str::to_string)
            .unwrap_or(name),
        None => name,
    };
    Ok(Value::str(&name))
}

fn dirname(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let dir = match Path::new(&p).parent() {
        Some(d) if !d.as_os_str().is_empty() => d.display().to_string(),
        _ => ".".to_string(),
    };
    Ok(Value::str(&dir))
}

fn extname(_vm: &mut Vm, _this: Value, args: Vec<Value>) -> Result<Value, Value> {
    let p = path_arg(&args);
    let ext = Path::new(&p)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    Ok(Value::str(&ext))
}
