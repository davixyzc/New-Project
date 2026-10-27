//! Runtime values and the conversion and comparison rules that operate on them.

use std::cell::{Cell, RefCell, RefMut};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::bytecode::Proto;
use crate::collection::Collection;
use crate::vm::Vm;

/// The number of changes that move the values of an object, change its shape, or free an object. A cache of the address of
/// a property's value, which native code keeps, holds while this count stays the same (see `epoch_address`).
static STRUCT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Counts one change to the values or the shape of an object, or the freeing of one.
fn bump_epoch() {
    use std::sync::atomic::Ordering::Relaxed;
    STRUCT_EPOCH.store(STRUCT_EPOCH.load(Relaxed).wrapping_add(1), Relaxed);
}

/// The address of the count of structural changes, which native code reads to check the caches it keeps.
pub fn epoch_address() -> u64 {
    &STRUCT_EPOCH as *const std::sync::atomic::AtomicU64 as u64
}

/// The own properties of an object, in the order they were added. Objects that add the same keys in the same order share
/// a `Shape`, which holds those keys, so a site that reads a property checks the shape of the object instead of the key
/// (see `PropHint`), and an object keeps only its values. An object with more keys than a shape holds, or one that loses
/// a property, keeps its own keys and an index by name instead (see `Dict`).
pub struct Props {
    /// The shape of an object in shape mode. In dictionary mode it is `dict_shape()`, which no cache holds.
    shape: &'static Shape,
    /// What a cache compares: the address of the shape in shape mode, and in dictionary mode an odd number that no other
    /// object gets. It changes when the position of a key does (see `remove`), so a cache that holds it is never wrong.
    id: usize,
    /// The values, in the order of the keys.
    values: Vec<Value>,
    /// The keys and the index of an object in dictionary mode.
    dict: Option<Box<Dict>>,
    /// The entries of a Map or a Set, for an object that is one (see `builtins::collections`).
    pub collection: Option<Box<Collection>>,
    /// The object that a lookup continues with when this one does not have the name: the prototype of an instance.
    pub proto: Option<ObjRef>,
    /// The accessor properties, which a lookup finds after the data properties of the object.
    pub accessors: Option<Box<Vec<Accessor>>>,
}

/// The keys of the objects that have them in this order. A shape is never freed: every shape hangs off a root through
/// the `next` lists, so a cache that holds the address of a shape never holds the address of a freed one.
pub struct Shape {
    keys: Vec<Rc<str>>,
    /// The shapes with one key more, each made of this shape's keys and one more.
    next: RefCell<Vec<&'static Shape>>,
}

/// The keys of an object in dictionary mode, each with its position.
#[derive(Clone, Default)]
struct Dict {
    keys: Vec<Rc<str>>,
    index: NameMap<usize>,
}

/// What a site remembers about a property that it reads or writes: the shape of the object where it last found the
/// property, and the position it found there. Another object with that shape has the property at that position too.
/// `index` is also the position of a built-in method of an array or a string, which a lookup finds the same way.
#[derive(Clone, Default)]
#[repr(C)]
pub struct PropHint {
    pub index: Cell<u32>,
    pub shape: Cell<usize>,
    /// The shape that a site which adds this key last added it to, and the shape that it made (see `Props::push_hinted`).
    pub from: Cell<usize>,
    pub to: Cell<usize>,
}

/// A getter and a setter under one name. Either of them may be missing.
#[derive(Clone)]
pub struct Accessor {
    pub name: Rc<str>,
    pub get: Option<Value>,
    pub set: Option<Value>,
}

/// An object in shape mode has at most this many keys. One more key turns it into a dictionary.
const SHAPE_KEYS: usize = 16;

thread_local! {
    /// The number of the next dictionary identity (see `Props::id`), which is odd and never given out twice.
    static NEXT_DICT_ID: Cell<usize> = const { Cell::new(1) };
    /// The shape of an object with no keys, which every new object starts from.
    static ROOT: &'static Shape = leak_shape(Vec::new());
    /// The shape of an object in dictionary mode. No cache holds it, since such an object has its own keys.
    static DICT_SHAPE: &'static Shape = leak_shape(Vec::new());
}

/// A shape made for good. Shapes are never freed, since each hangs off the root through the `next` lists, so an object
/// refers to its shape without counting it.
fn leak_shape(keys: Vec<Rc<str>>) -> &'static Shape {
    Box::leak(Box::new(Shape {
        keys,
        next: RefCell::new(Vec::new()),
    }))
}

fn root_shape() -> &'static Shape {
    ROOT.with(|shape| *shape)
}

fn dict_shape() -> &'static Shape {
    DICT_SHAPE.with(|shape| *shape)
}

/// The identity of a shape in shape mode: its address, which the id of an object holds (see `Props::id`).
fn shape_id(shape: &'static Shape) -> usize {
    shape as *const Shape as usize
}

/// A new identity for a dictionary-mode object.
fn next_dict_id() -> usize {
    NEXT_DICT_ID.with(|next| {
        let id = next.get();
        next.set(id + 2);
        id
    })
}

impl Shape {
    /// The shape with `key` added after the keys of this one. It is found among the shapes that follow this one, or it
    /// is made and added to them.
    fn with_key(&'static self, key: &Rc<str>) -> &'static Shape {
        // Interned keys share their allocation, so the pointer finds almost every child; the text is compared only when
        // the pointer finds none.
        let next = self.next.borrow();
        let last = |s: &&'static Shape| s.keys.last().cloned();
        let found = next
            .iter()
            .find(|s| last(s).is_some_and(|k| Rc::ptr_eq(&k, key)))
            .or_else(|| next.iter().find(|s| last(s).is_some_and(|k| *k == **key)))
            .copied();
        drop(next);
        if let Some(shape) = found {
            return shape;
        }
        let mut keys = self.keys.clone();
        keys.push(key.clone());
        let shape = leak_shape(keys);
        self.next.borrow_mut().push(shape);
        shape
    }
}

impl Drop for Props {
    fn drop(&mut self) {
        bump_epoch();
    }
}

impl Clone for Props {
    /// A copy has its own dictionary, which may change apart from this one, so it gets an identity of its own.
    fn clone(&self) -> Props {
        Props {
            shape: self.shape,
            id: if self.dict.is_some() {
                next_dict_id()
            } else {
                self.id
            },
            values: self.values.clone(),
            dict: self.dict.clone(),
            collection: self.collection.clone(),
            proto: self.proto.clone(),
            accessors: self.accessors.clone(),
        }
    }
}

impl Default for Props {
    fn default() -> Props {
        Props::with_capacity(0)
    }
}

impl Props {
    pub fn with_capacity(capacity: usize) -> Props {
        let shape = root_shape();
        Props {
            id: shape_id(shape),
            shape,
            values: Vec::with_capacity(capacity),
            dict: None,
            collection: None,
            proto: None,
            accessors: None,
        }
    }

    /// The keys, in the order the properties were added.
    fn keys(&self) -> &[Rc<str>] {
        match &self.dict {
            Some(dict) => &dict.keys,
            None => &self.shape.keys,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Each own property, with its key, in the order the properties were added.
    pub fn iter(&self) -> impl Iterator<Item = (&Rc<str>, &Value)> {
        self.keys().iter().zip(&self.values)
    }

    pub fn key(&self, i: usize) -> &Rc<str> {
        &self.keys()[i]
    }

    pub fn value(&self, i: usize) -> &Value {
        &self.values[i]
    }

    pub fn set_value(&mut self, i: usize, v: Value) {
        self.values[i] = v;
    }

    /// The position of the property called `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        match &self.dict {
            Some(dict) => dict.index.get(name).copied(),
            None => self.shape.keys.iter().position(|k| &**k == name),
        }
    }

    /// The position of the property with the shared key `key`. The pointer is compared first, which finds almost every
    /// key, and the text only when the pointer finds nothing.
    pub fn position_of_key(&self, key: &Rc<str>) -> Option<usize> {
        match &self.dict {
            Some(dict) => dict.index.get(&**key).copied(),
            None => self
                .shape
                .keys
                .iter()
                .position(|k| Rc::ptr_eq(k, key))
                .or_else(|| self.shape.keys.iter().position(|k| **k == **key)),
        }
    }

    /// The position of `key`, looked up through `hint` first. An object in shape mode with the shape that the hint holds
    /// has the key at the position that the hint holds. Any other lookup of such an object sets the hint to what it finds.
    pub fn position_hinted(&self, key: &Rc<str>, hint: &PropHint) -> Option<usize> {
        if hint.shape.get() == self.id {
            return Some(hint.index.get() as usize);
        }
        let i = self.position_of_key(key)?;
        hint.shape.set(self.id);
        hint.index.set(i as u32);
        Some(i)
    }

    /// The accessor property called `name` of this object, not of its prototype.
    pub fn accessor(&self, name: &str) -> Option<&Accessor> {
        self.accessors.as_ref()?.iter().find(|a| &*a.name == name)
    }

    /// Sets the getter, or the setter when `setter` is true, of the accessor property `name`, which is made when it is
    /// not there yet.
    pub fn set_accessor(&mut self, name: &Rc<str>, setter: bool, f: Value) {
        let list = self.accessors.get_or_insert_with(Default::default);
        let i = match list.iter().position(|a| *a.name == **name) {
            Some(i) => i,
            None => {
                list.push(Accessor {
                    name: name.clone(),
                    get: None,
                    set: None,
                });
                list.len() - 1
            }
        };
        if setter {
            list[i].set = Some(f);
        } else {
            list[i].get = Some(f);
        }
    }

    /// Removes the own property `name`, and tells whether there was one. The object turns to dictionary mode.
    pub fn remove(&mut self, name: &str) -> bool {
        let Some(i) = self.position(name) else {
            return false;
        };
        self.make_dict();
        self.id = next_dict_id();
        bump_epoch();
        self.values.remove(i);
        if let Some(dict) = &mut self.dict {
            dict.keys.remove(i);
            dict.index = dict
                .keys
                .iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), i))
                .collect();
        }
        true
    }

    /// Adds a property, which the caller has checked is not there yet.
    pub fn push(&mut self, key: Rc<str>, v: Value) {
        self.push_hinted(key, v, None);
    }

    /// `push`, which records in `hint` the shape transition that it makes, so that a site which adds the same key to
    /// objects of the same shape can make the same transition without looking it up.
    fn push_hinted(&mut self, key: Rc<str>, v: Value, hint: Option<&PropHint>) {
        bump_epoch();
        if self.dict.is_none() && self.shape.keys.len() < SHAPE_KEYS {
            let from = self.id;
            self.shape = self.shape.with_key(&key);
            self.id = shape_id(self.shape);
            if let Some(hint) = hint {
                hint.from.set(from);
                hint.to.set(self.id);
            }
        } else {
            self.make_dict();
            if let Some(dict) = &mut self.dict {
                dict.index.insert(key.clone(), dict.keys.len());
                dict.keys.push(key);
            }
        }
        self.values.push(v);
    }

    /// Turns the object into dictionary mode, which keeps its own keys and their index.
    fn make_dict(&mut self) {
        if self.dict.is_none() {
            bump_epoch();
            let keys = self.shape.keys.clone();
            let index = keys
                .iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), i))
                .collect();
            self.dict = Some(Box::new(Dict { keys, index }));
            self.shape = dict_shape();
            self.id = next_dict_id();
        }
    }
}

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
    /// The captured variables: a `Value::Cell` for a variable that is shared with the enclosing frame, or the value
    /// itself for a parameter copied in (see `UpvalDesc::by_value`).
    pub upvals: Vec<Value>,
    /// `this` captured at creation time; only used by arrow functions.
    pub this: Value,
    /// What the function holds as a class or as a constructor (see `ClassInfo`). Most functions hold none.
    pub class: RefCell<Option<Box<ClassInfo>>>,
}

/// What a class, or a function that is used as a constructor, holds beyond its code.
#[derive(Default)]
pub struct ClassInfo {
    /// The object that the method was defined on: the prototype for an instance method, and the statics for a static
    /// one. `super` looks up the property on the prototype of this object.
    pub home: Option<ObjRef>,
    /// The prototype of the objects that `new` makes, which holds the methods. It is made when it is first needed.
    pub prototype: Option<ObjRef>,
    /// The static members, which are looked up on the function.
    pub statics: Option<ObjRef>,
    /// The superclass, which `super(...)` calls.
    pub parent: Option<Value>,
}

impl Closure {
    /// The class data, made when it is first changed.
    pub fn info_mut(&self) -> RefMut<'_, ClassInfo> {
        RefMut::map(self.class.borrow_mut(), |c| {
            &mut **c.get_or_insert_with(Default::default)
        })
    }

    /// The prototype of the objects that `new` makes with this function. It is made when it is first needed, and it
    /// has the function as its `constructor`.
    pub fn instance_proto(self: &Rc<Self>) -> ObjRef {
        let existing = self.info_mut().prototype.clone();
        if let Some(p) = existing {
            return p;
        }
        let p = new_obj(None);
        obj_put(&p, &intern_name("constructor"), Value::Func(self.clone()));
        self.info_mut().prototype = Some(p.clone());
        p
    }

    /// The static members of the function, made when they are first set.
    pub fn statics(&self) -> ObjRef {
        let existing = self.info_mut().statics.clone();
        if let Some(s) = existing {
            return s;
        }
        let s = new_obj(None);
        self.info_mut().statics = Some(s.clone());
        s
    }
}

/// The text of a string value. It keeps its character count and whether every character is ASCII, so `length` and
/// indexing take constant time for ASCII text, which is the usual case.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JsStr {
    text: String,
    chars: usize,
    ascii: bool,
}

impl JsStr {
    pub fn new(text: String) -> JsStr {
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        JsStr { text, chars, ascii }
    }

    /// The length in characters.
    pub fn char_count(&self) -> usize {
        self.chars
    }

    /// Whether every character is ASCII, so that a character index is also a byte index.
    pub fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// Appends `more`, keeping the counts up to date.
    pub fn push_str(&mut self, more: &str) {
        self.chars += if more.is_ascii() {
            more.len()
        } else {
            more.chars().count()
        };
        self.ascii = self.ascii && more.is_ascii();
        self.text.push_str(more);
    }
}

impl std::ops::Deref for JsStr {
    type Target = String;

    fn deref(&self) -> &String {
        &self.text
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// The one-character string for `c`. ASCII characters share cached values, so indexing a string does not allocate.
pub fn char_value(c: char) -> Value {
    thread_local! {
        static ASCII: Vec<Rc<JsStr>> = (0u8..128)
            .map(|b| Rc::new(JsStr::new((b as char).to_string())))
            .collect();
    }
    if c.is_ascii() {
        ASCII.with(|table| Value::Str(Rc::clone(&table[c as usize])))
    } else {
        Value::str(c.encode_utf8(&mut [0; 4]))
    }
}

#[derive(Clone)]
#[repr(u8)]
pub enum Value {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<JsStr>),
    Arr(ArrRef),
    Obj(ObjRef),
    Func(Rc<Closure>),
    Native(&'static NativeDef),
    /// A boxed local. It sits in a frame slot or in the upvalues of a closure, and is never visible to user code.
    Cell(Rc<RefCell<Value>>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::new(JsStr::new(s.to_string())))
    }

    /// A string value that takes over `s`, without copying it.
    pub fn from_string(s: String) -> Value {
        Value::Str(Rc::new(JsStr::new(s)))
    }

    pub fn arr(items: Vec<Value>) -> Value {
        Value::Arr(Rc::new(RefCell::new(items)))
    }

    /// A new Map or Set, which holds the entries `c`.
    pub fn collection(c: Collection) -> Value {
        let mut props = Props::default();
        props.collection = Some(Box::new(c));
        Value::Obj(Rc::new(RefCell::new(props)))
    }

    pub fn obj() -> Value {
        Value::obj_with(0)
    }

    pub fn obj_with(capacity: usize) -> Value {
        Value::Obj(Rc::new(RefCell::new(Props::with_capacity(capacity))))
    }

    /// A new object whose prototype is `proto`, which is what `new` makes for a constructor.
    pub fn obj_with_proto(proto: ObjRef) -> Value {
        Value::Obj(new_obj(Some(proto)))
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
    match props.position(name) {
        Some(i) => props.set_value(i, v),
        None => props.push(Rc::from(name), v),
    }
}

/// Sets an own property using a key that is already shared, so no string is copied.
pub fn obj_put(obj: &ObjRef, key: &Rc<str>, v: Value) {
    let mut props = obj.borrow_mut();
    match props.position_of_key(key) {
        Some(i) => props.set_value(i, v),
        None => props.push(key.clone(), v),
    }
}

/// What a property lookup finds along a prototype chain.
pub enum Lookup {
    /// A data property, or the method or the size of a Map or a Set.
    Data(Value),
    /// An accessor with a getter. The caller runs it, with the object the lookup started from as `this`.
    Getter(Value),
    /// No object on the chain has the name.
    Missing,
}

/// The property `name` of one object, not counting its prototype. None when the object does not have it.
fn own_lookup(props: &Props, name: &str) -> Option<Lookup> {
    if let Some(i) = props.position(name) {
        return Some(Lookup::Data(props.value(i).clone()));
    }
    if let Some(v) = crate::builtins::collections::property(props, name) {
        return Some(Lookup::Data(v));
    }
    let accessor = props.accessor(name)?;
    Some(match &accessor.get {
        Some(getter) => Lookup::Getter(getter.clone()),
        None => Lookup::Data(Value::Undef),
    })
}

/// The property `name` of `obj`, looked up on the object and then along its prototype chain.
pub fn lookup(obj: &ObjRef, name: &str) -> Lookup {
    let mut cur = obj.clone();
    loop {
        let next = {
            let props = cur.borrow();
            if let Some(found) = own_lookup(&props, name) {
                return found;
            }
            props.proto.clone()
        };
        match next {
            Some(p) => cur = p,
            None => return end_of_chain(name),
        }
    }
}

/// What a lookup finds once the prototype chain of an object has ended: the methods of `Object.prototype`.
fn end_of_chain(name: &str) -> Lookup {
    match crate::builtins::global::object_method(name) {
        Some(def) => Lookup::Data(Value::Native(def)),
        None => Lookup::Missing,
    }
}

/// `lookup` for a property read by native code, which tries the position in `hint` first on `obj` itself (see
/// `Props::position_hinted`). Names from the same source share one allocation (see `intern_name`), so the pointer
/// decides almost every lookup.
pub fn lookup_hinted(obj: &ObjRef, key: &Rc<str>, hint: &PropHint) -> Lookup {
    let next = {
        let props = obj.borrow();
        if let Some(i) = props.position_hinted(key, hint) {
            return Lookup::Data(props.value(i).clone());
        }
        if let Some(found) = own_lookup(&props, key) {
            return found;
        }
        props.proto.clone()
    };
    match next {
        Some(p) => lookup(&p, key),
        None => end_of_chain(key),
    }
}

/// The setter that an assignment to `name` on `obj` runs, when the name is an accessor on the object or on its chain
/// before any data property of that name. `Some(None)` is an accessor without a setter, which ignores the assignment.
/// None means that the assignment stores a data property on `obj` itself.
pub fn find_setter(obj: &ObjRef, name: &str) -> Option<Option<Value>> {
    let mut cur = obj.clone();
    loop {
        let next = {
            let props = cur.borrow();
            if props.position(name).is_some() {
                return None;
            }
            if let Some(accessor) = props.accessor(name) {
                return Some(accessor.set.clone());
            }
            props.proto.clone()
        };
        match next {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// A new object with no own properties, whose prototype is `proto`.
pub fn new_obj(proto: Option<ObjRef>) -> ObjRef {
    let mut props = Props::default();
    props.proto = proto;
    Rc::new(RefCell::new(props))
}

/// The prototypes of the built-in errors, by kind. Each kind inherits from the prototype of `Error`, which is first.
pub fn error_protos() -> Vec<(&'static str, ObjRef)> {
    let base = new_obj(None);
    let mut out = vec![("Error", base.clone())];
    for kind in ["TypeError", "RangeError", "SyntaxError", "ReferenceError"] {
        out.push((kind, new_obj(Some(base.clone()))));
    }
    out
}

/// Sets the `name`, `message` and `stack` of an error object.
pub fn init_error(obj: &ObjRef, kind: &str, msg: &str, stack: &str) {
    obj_put(obj, &intern_name("name"), Value::str(kind));
    obj_put(obj, &intern_name("message"), Value::str(msg));
    obj_put(obj, &intern_name("stack"), Value::str(stack));
}

/// The stack text of an error: its header, then the location it was made at when there is one.
pub fn stack_text(kind: &str, msg: &str, loc: &str) -> String {
    let mut stack = String::with_capacity(kind.len() + msg.len() + loc.len() + 16);
    stack.push_str(kind);
    stack.push_str(": ");
    stack.push_str(msg);
    if !loc.is_empty() {
        stack.push_str("\n    at ");
        stack.push_str(loc);
    }
    stack
}

/// The name of the class of an object, which is the name of the `constructor` of its prototype. None when the object
/// has no prototype with a named constructor.
pub fn class_name(props: &Props) -> Option<String> {
    let proto = props.proto.as_ref()?;
    let p = proto.borrow();
    match p.position("constructor").map(|i| p.value(i)) {
        Some(Value::Func(c)) if !c.proto.name.is_empty() => Some(c.proto.name.clone()),
        _ => None,
    }
}

/// `obj_put`, which tries the position in `hint` first (see `Props::position_hinted`).
pub fn obj_put_hinted(obj: &ObjRef, key: &Rc<str>, v: Value, hint: &PropHint) {
    let mut props = obj.borrow_mut();
    match props.position_hinted(key, hint) {
        Some(i) => props.set_value(i, v),
        None => props.push(key.clone(), v),
    }
}

/// Where the properties of an object sit in memory, for native code that reads and writes one in place (see
/// `jit::codegen`). `shape` is from the allocation of the object to the address of its shape. `values` is from there to
/// the `Vec` of its values, whose `buf` and `len` are the pointer and the length within that `Vec`.
#[derive(Clone, Copy)]
pub(crate) struct PropLayout {
    pub shape: i32,
    pub shape_rc: i32,
    pub values: i32,
    pub buf: i32,
    pub len: i32,
    pub cap: i32,
}

/// The layout of the properties of an object, when native code can rely on it. It is found once, by probing the layout
/// that Rust gives these types. None when a probe does not find what native code expects, and then the helpers do the
/// work alone.
pub(crate) fn prop_layout() -> Option<PropLayout> {
    static LAYOUT: std::sync::OnceLock<Option<PropLayout>> = std::sync::OnceLock::new();
    *LAYOUT.get_or_init(probe_prop_layout)
}

fn probe_prop_layout() -> Option<PropLayout> {
    use std::mem::{offset_of, size_of};
    let arr = arr_layout()?;
    if size_of::<usize>() != size_of::<&'static Shape>() {
        return None;
    }
    // The properties of an object sit in a `RefCell`, whose value Rust does not place at a fixed offset.
    let cell = RefCell::new(Props::default());
    let cell_value = cell.as_ptr() as usize - &cell as *const _ as usize;
    let base = crate::jit::RC_VALUE + cell_value as i32;
    Some(PropLayout {
        shape: base + offset_of!(Props, id) as i32,
        shape_rc: base + offset_of!(Props, shape) as i32,
        values: base + offset_of!(Props, values) as i32,
        buf: arr.buf,
        len: arr.len,
        cap: arr.cap,
    })
}

/// Where the entries of a `Vec` of names and values sit in memory, for native code that reads the globals (see
/// `jit::codegen`). `buf` and `len` are the pointer and the length within the `Vec`, and each entry is `entry` bytes long,
/// with its name at `key` and its value at `value`.
#[derive(Clone, Copy)]
pub(crate) struct EntryLayout {
    pub buf: i32,
    pub len: i32,
    pub entry: i32,
    pub key: i32,
    pub value: i32,
}

/// The layout of the entries of the globals, which `prop_layout` relies on for its `Vec` as well.
pub(crate) fn entry_layout() -> Option<EntryLayout> {
    static LAYOUT: std::sync::OnceLock<Option<EntryLayout>> = std::sync::OnceLock::new();
    *LAYOUT.get_or_init(|| {
        use std::mem::{offset_of, size_of};
        let arr = arr_layout()?;
        Some(EntryLayout {
            buf: arr.buf,
            len: arr.len,
            entry: size_of::<(Rc<str>, Value)>() as i32,
            key: offset_of!((Rc<str>, Value), 0) as i32,
            value: offset_of!((Rc<str>, Value), 1) as i32,
        })
    })
}

/// Where the elements of an array sit in memory, for native code (see `jit::codegen`). `vec` is from the allocation of
/// the array to the `Vec` of its elements, and `buf` and `len` are the pointer and the length within that `Vec`.
#[derive(Clone, Copy)]
pub(crate) struct ArrLayout {
    pub vec: i32,
    pub buf: i32,
    pub len: i32,
    pub cap: i32,
}

/// The offset from the allocation of an `Rc<RefCell<Value>>` (a captured variable) to the value that it holds, when
/// native code can rely on it. It is found once, as `prop_layout` is.
pub(crate) fn cell_value_offset() -> Option<i32> {
    static OFFSET: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        if !crate::jit::rc_layout_ok() {
            return None;
        }
        let cell = RefCell::new(Value::Undef);
        Some(crate::jit::RC_VALUE + (cell.as_ptr() as usize - &cell as *const _ as usize) as i32)
    })
}

/// The layout of the elements of an array, when native code can rely on it. It is found once, as `prop_layout` is.
pub(crate) fn arr_layout() -> Option<ArrLayout> {
    static LAYOUT: std::sync::OnceLock<Option<ArrLayout>> = std::sync::OnceLock::new();
    *LAYOUT.get_or_init(probe_arr_layout)
}

fn probe_arr_layout() -> Option<ArrLayout> {
    use std::mem::size_of;
    if !crate::jit::rc_layout_ok()
        || size_of::<Vec<Value>>() != 3 * size_of::<usize>()
        || size_of::<Value>() != 16
    {
        return None;
    }
    // Five slots, of which three are used, so that the length and the capacity differ.
    let cell = RefCell::new(Vec::with_capacity(5));
    cell.borrow_mut()
        .extend([Value::Undef, Value::Undef, Value::Undef]);
    let cell_value = cell.as_ptr() as usize - &cell as *const _ as usize;
    let (ptr, len, cap) = {
        let items = cell.borrow();
        (items.as_ptr() as usize, items.len(), items.capacity())
    };
    // SAFETY: the `Vec` is the three words that the size check above confirms, and each is read as a `usize`.
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(&*cell.as_ptr()) };
    let word_of = |want: usize| {
        words
            .iter()
            .position(|&w| w == want)
            .map(|i| (i * size_of::<usize>()) as i32)
    };
    Some(ArrLayout {
        vec: crate::jit::RC_VALUE + cell_value as i32,
        buf: word_of(ptr)?,
        len: word_of(len)?,
        cap: word_of(cap)?,
    })
}

/// `obj_put` for a property that an object literal adds, which records the shape transition in `hint` (see
/// `Props::push_hinted`).
pub fn obj_init_hinted(obj: &ObjRef, key: &Rc<str>, v: Value, hint: &PropHint) {
    let mut props = obj.borrow_mut();
    match props.position_of_key(key) {
        Some(i) => props.set_value(i, v),
        None => props.push_hinted(key.clone(), v, Some(hint)),
    }
}

/// A hasher for names, which are short: a rotate and a multiply per byte. The default hasher is built for long keys
/// and is slow on short ones, and a built-in such as `Math` is looked up by name each time it is used.
#[derive(Default)]
pub struct NameHasher(u64);

impl std::hash::Hasher for NameHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
}

/// A table keyed by name, using the name hasher.
pub type NameMap<V> = HashMap<Rc<str>, V, std::hash::BuildHasherDefault<NameHasher>>;

thread_local! {
    static NAMES: RefCell<NameMap<()>> = RefCell::new(Default::default());
}

/// The shared allocation for a name in the source, so that every use of `name` is the same `Rc`, and a property
/// lookup by pointer finds it. Only names in the source are interned: their number is bounded by the program text.
pub fn intern_name(name: &str) -> Rc<str> {
    NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some((shared, _)) = names.get_key_value(name) {
            return shared.clone();
        }
        let shared: Rc<str> = Rc::from(name);
        names.insert(shared.clone(), ());
        shared
    })
}

/// Reads an own property of a plain object.
pub fn obj_get(obj: &ObjRef, name: &str) -> Option<Value> {
    let props = obj.borrow();
    match props.position(name) {
        Some(i) => Some(props.value(i).clone()),
        None => crate::builtins::collections::property(&props, name),
    }
}

/// Builds a plain object from name/value pairs.
pub fn make_obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut props = Props::default();
    for (k, v) in pairs {
        props.push(intern_name(k), v);
    }
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

/// `ToInt32`: the number as the 32-bit integer that the bitwise operators work on. Infinities and NaN are 0, and
/// anything else is truncated and wrapped modulo 2^32.
pub fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4_294_967_296.0) as u32) as i32
}

pub fn to_num(v: &Value) -> f64 {
    match v {
        Value::Undef => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Num(n) => *n,
        Value::Str(s) => text_to_num(s),
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

/// Formats a number the way JavaScript's `Number::toString` does: the shortest digits that read back as the same
/// number, in positional form up to 21 digits and in exponent form beyond that.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        let mut text = String::with_capacity(20);
        push_int(&mut text, n as i64);
        return text;
    }
    let mut text = String::new();
    if push_short_decimal(&mut text, n) {
        return text;
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let (digits, point) = shortest_digits(n.abs());
    let k = digits.len() as i32;
    let body = if k <= point && point <= 21 {
        format!("{}{}", digits, "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else if -6 < point && point <= 0 {
        format!("0.{}{}", "0".repeat((-point) as usize), digits)
    } else {
        let e = point - 1;
        let exp_sign = if e < 0 { "-" } else { "+" };
        if k == 1 {
            format!("{}e{}{}", digits, exp_sign, e.abs())
        } else {
            format!("{}.{}e{}{}", &digits[..1], &digits[1..], exp_sign, e.abs())
        }
    };
    format!("{}{}", sign, body)
}

/// The shortest digits that read back as `x`, which is positive and finite, and the position of the decimal point:
/// `x` is `0.d1d2...dk` times ten to the power `point`. Rust rounds an exact expansion to the even digit when two
/// candidates of that length are equally close, which is the one JavaScript picks, so that rounding is used when
/// it still reads back as `x`.
fn shortest_digits(x: f64) -> (String, i32) {
    let short = format!("{:e}", x);
    let (mantissa, exp) = short.split_once('e').unwrap_or((short.as_str(), "0"));
    let mut digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut exp10: i32 = exp.parse().unwrap_or(0);
    if could_tie(x) {
        let exact = format!("{:.*e}", digits.len() - 1, x);
        if let Some((m, e)) = exact.split_once('e') {
            let alt: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
            if alt != digits && exact.parse::<f64>() == Ok(x) {
                digits = alt;
                exp10 = e.parse().unwrap_or(exp10);
            }
        }
    }
    (digits.trim_end_matches('0').to_string(), exp10 + 1)
}

/// Whether two shortest candidates of `x` can tie. A tie needs the exact decimal form of `x` to have one more
/// significant digit than the candidates, which are at most 17 digits, so an exact form of more than 18 digits
/// rules it out. A double `m * 2^e` with `e < 0` has the exact form `m * 5^-e` times `10^e`.
fn could_tie(x: f64) -> bool {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let zeros = m.trailing_zeros();
    m >>= zeros;
    e += zeros as i32;
    if e >= 0 {
        return false;
    }
    let j = (-e) as u32;
    // `m` is below 2^53, so `m * 5^27` still fits in a u128.
    j <= 27 && (u128::from(m) * 5u128.pow(j)).to_string().len() <= 18
}

pub fn to_str(v: &Value) -> String {
    to_str_in(v, None)
}

/// `to_str`, where `joining` is the chain of arrays whose text is still being built further out.
fn to_str_in(v: &Value, joining: Option<&Joining>) -> String {
    match v {
        Value::Undef => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => num_to_string(*n),
        Value::Str(s) => s.to_string(),
        Value::Arr(a) => join_items(a, ",", joining),
        Value::Obj(o) => {
            let tag = o.borrow().collection.as_ref().map(|c| {
                if c.is_set {
                    "[object Set]"
                } else {
                    "[object Map]"
                }
            });
            match tag {
                Some(tag) => tag.to_string(),
                None => error_text(o).unwrap_or_else(|| "[object Object]".to_string()),
            }
        }
        Value::Func(_) | Value::Native(_) => "function".to_string(),
        Value::Cell(c) => to_str_in(&c.borrow(), joining),
    }
}

/// An array whose text is being built, and the arrays it sits in. Each one lives on the stack of its `join_items` call.
pub struct Joining<'a> {
    id: *const RefCell<Vec<Value>>,
    outer: Option<&'a Joining<'a>>,
}

/// Joins the items of an array with `sep`; a null or undefined item is empty. An array that is already being joined
/// further out is empty too, so an array that contains itself ends there, as in `Array.prototype.join`.
pub fn join_items(a: &ArrRef, sep: &str, outer: Option<&Joining>) -> String {
    let id = Rc::as_ptr(a);
    let mut link = outer;
    while let Some(j) = link {
        if j.id == id {
            return String::new();
        }
        link = j.outer;
    }
    let joining = Joining { id, outer };
    let items = a.borrow();
    let mut text = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            text.push_str(sep);
        }
        match item {
            Value::Undef | Value::Null => {}
            Value::Str(s) => text.push_str(s),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => push_int(&mut text, *n as i64),
            other => text.push_str(&to_str_in(other, Some(&joining))),
        }
    }
    text
}

/// `Number(text)` for a string: decimal numbers, `0x`, `0o` and `0b` integers, and the names `Infinity` and
/// `-Infinity`. Anything else, such as `inf` or `nan`, is not a number.
fn text_to_num(text: &str) -> f64 {
    let t = text.trim();
    if t.is_empty() {
        return 0.0;
    }
    let radix: u32 = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 0,
    };
    if radix != 0 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return digits.chars().fold(0.0, |acc, c| {
            acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
        });
    }
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if t
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E')) =>
        {
            t.parse::<f64>().unwrap_or(f64::NAN)
        }
        _ => f64::NAN,
    }
}

/// `String(error)` for an error object, which `make_error` gives `name`, `message` and `stack`. Any other object
/// prints as `[object Object]`.
fn error_text(o: &ObjRef) -> Option<String> {
    let props = o.borrow();
    let get = |key: &str| props.iter().find(|&(k, _)| &**k == key).map(|(_, v)| v);
    get("stack")?;
    let name = to_str(get("name")?);
    let message = to_str(get("message")?);
    Some(match (name.is_empty(), message.is_empty()) {
        (_, true) => name,
        (true, false) => message,
        (false, false) => format!("{}: {}", name, message),
    })
}

/// The positions of an object's properties in the order JavaScript lists them: keys that are array indices come
/// first, in ascending order, and the others keep their insertion order.
pub fn key_order(props: &Props) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut names = Vec::new();
    for (i, (k, _)) in props.iter().enumerate() {
        match array_index(k) {
            Some(n) => indices.push((n, i)),
            None => names.push(i),
        }
    }
    // Without index keys the insertion order is the order, and nothing more needs building.
    if indices.is_empty() {
        return names;
    }
    indices.sort_unstable();
    indices.into_iter().map(|(_, i)| i).chain(names).collect()
}

/// Whether `key` is an array index, which JavaScript lists before the other keys of an object.
pub fn is_index_key(key: &str) -> bool {
    array_index(key).is_some()
}

/// An array index spelled the way JavaScript spells it: digits with no leading zero, below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    (n < u64::from(u32::MAX)).then_some(n as u32)
}

/// The `+` operator: numeric addition, or string concatenation when either side is not a number.
pub fn add(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
        (Value::Str(x), Value::Str(y)) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Value::from_string(s)
        }
        _ if is_textual(a) || is_textual(b) => {
            let mut s = String::new();
            append_text(&mut s, a);
            append_text(&mut s, b);
            Value::from_string(s)
        }
        _ => Value::Num(to_num(a) + to_num(b)),
    }
}

/// Appends the text of `v`, which is how `+` and template strings read a value, to `s`. Numbers that are whole are
/// written straight into `s`, without a temporary string.
fn append_text(s: &mut String, v: &Value) {
    match v {
        Value::Str(x) => s.push_str(x),
        Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => push_int(s, *n as i64),
        other => s.push_str(&to_str(other)),
    }
}

/// Appends the JavaScript text of a finite number. An integer, and a number with few digits after the point, are written
/// without a temporary string.
pub fn push_number(out: &mut String, n: f64) {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        push_int(out, n as i64);
    } else if !push_short_decimal(out, n) {
        out.push_str(&num_to_string(n));
    }
}

/// Appends `n` when its shortest text has few digits after the point, and tells whether it did. Such a number is `m / 10^d`
/// for a whole `m` and a small `d`, and that text reads back as `n` exactly when the division gives `n`, since both
/// operands are exact. The gap to the next double bounds how close two such texts can be: while `10^-d` is wider than
/// that gap, at most one text reads back as `n`, and it is the one JavaScript writes.
fn push_short_decimal(out: &mut String, n: f64) -> bool {
    let x = n.abs();
    // Below 1e-6 JavaScript writes an exponent, and from 2^53 on a scaled whole number is no longer exact.
    if !(1e-6..9.007_199_254_740_992e15).contains(&x) {
        return false;
    }
    let gap = f64::from_bits(x.to_bits() + 1) - x;
    let mut scale = 1.0_f64;
    for d in 1..=8u32 {
        scale *= 10.0;
        if 1.0 / scale <= gap {
            break;
        }
        let m = (x * scale).round();
        if m / scale == x {
            if n < 0.0 {
                out.push('-');
            }
            let unit = 10u64.pow(d);
            let frac = m as u64 % unit;
            push_int(out, (m as u64 / unit) as i64);
            out.push('.');
            for place in (0..d).rev() {
                out.push(char::from(b'0' + (frac / 10u64.pow(place) % 10) as u8));
            }
            return true;
        }
    }
    false
}

/// The two digits of each number from 00 to 99, so that an integer is written two digits at a time.
const DIGIT_PAIRS: [[u8; 2]; 100] = {
    let mut pairs = [[0u8; 2]; 100];
    let mut i = 0;
    while i < 100 {
        pairs[i] = [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
        i += 1;
    }
    pairs
};

/// The decimal digits of `v`, with a minus sign when it is negative, written at the end of `buf`.
fn int_text(v: i64, buf: &mut [u8; 21]) -> &str {
    let mut at = buf.len();
    let mut rest = v.unsigned_abs();
    while rest >= 100 {
        at -= 2;
        buf[at..at + 2].copy_from_slice(&DIGIT_PAIRS[(rest % 100) as usize]);
        rest /= 100;
    }
    if rest >= 10 {
        at -= 2;
        buf[at..at + 2].copy_from_slice(&DIGIT_PAIRS[rest as usize]);
    } else {
        at -= 1;
        buf[at] = b'0' + rest as u8;
    }
    if v < 0 {
        at -= 1;
        buf[at] = b'-';
    }
    // SAFETY: the bytes are ASCII digits and a minus sign, so they are valid UTF-8.
    unsafe { std::str::from_utf8_unchecked(&buf[at..]) }
}

/// Appends the decimal digits of `v`.
fn push_int(s: &mut String, v: i64) {
    let mut digits = [0u8; 21];
    s.push_str(int_text(v, &mut digits));
}

/// The longest text the string operations build, in bytes. It is V8's limit, which V8 counts in characters.
pub const MAX_STRING_LEN: usize = (1 << 29) - 24;

/// Whether joining `a` and `b` would give a string over `MAX_STRING_LEN`. Only string operands are measured,
/// since their size is known without converting them.
pub fn joins_too_long(a: &Value, b: &Value) -> bool {
    let size = |v: &Value| match v {
        Value::Str(s) => s.len(),
        _ => 0,
    };
    size(a) + size(b) > MAX_STRING_LEN
}

/// `old + rhs` where `old` is owned. A string held only by `old` is extended in place,
/// which keeps loops that build a string linear instead of quadratic.
pub fn concat_owned(old: Value, rhs: Value) -> Value {
    match old {
        Value::Str(mut s) => {
            if Rc::get_mut(&mut s).is_none() {
                let mut copy = String::with_capacity(s.len() * 2 + 16);
                copy.push_str(&s);
                s = Rc::new(JsStr::new(copy));
            }
            let buf = Rc::get_mut(&mut s).unwrap();
            match &rhs {
                Value::Str(r) => buf.push_str(r),
                Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => {
                    let mut digits = [0u8; 21];
                    buf.push_str(int_text(*n as i64, &mut digits));
                }
                other => buf.push_str(&to_str(other)),
            }
            Value::Str(s)
        }
        other => add(&other, &rhs),
    }
}

/// `%` with an integer fast path, because the floating-point remainder is slow.
pub fn js_mod(a: f64, b: f64) -> f64 {
    // Below 2^53 the quotient of the magnitudes, cut to a whole number, is exact in floating point, and the remainder
    // that follows from it is exact too. The casts check that both operands are whole.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    let (m, d) = (a.abs(), b.abs());
    if m < EXACT && (1.0..EXACT).contains(&d) && (m as i64) as f64 == m && (d as i64) as f64 == d {
        let r = m - (m / d) as i64 as f64 * d;
        return if a.is_sign_negative() { -r } else { r };
    }
    // Casts avoid the libm call that `fract` needs on baseline x86-64. Below 2^63 the operands are exact in i64, and
    // so is the remainder, which is also exact as a float: fmod's result always is. The bound keeps `i64::MIN % -1`
    // out.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let (ai, bi) = (a as i64, b as i64);
    if ai as f64 == a && bi as f64 == b && bi != 0 && a.abs() < LIMIT && b.abs() < LIMIT {
        let r = ai % bi;
        // A zero remainder takes the sign of the dividend, so `-4 % 2` is -0.
        if r == 0 && a.is_sign_negative() {
            -0.0
        } else {
            r as f64
        }
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
        _ if a.is_object() || b.is_object() => compare(&to_primitive(a), &to_primitive(b)),
        _ => to_num(a).partial_cmp(&to_num(b)),
    }
}

/// Objects, arrays and functions take their text, as `ToPrimitive` does before a relational comparison.
fn to_primitive(v: &Value) -> Value {
    if v.is_object() {
        Value::str(&to_str(v))
    } else {
        v.clone()
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
        (
            Value::Arr(_) | Value::Obj(_) | Value::Func(_),
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
        ) => loose_eq(&Value::str(&to_str(a)), b),
        (
            Value::Num(_) | Value::Str(_) | Value::Bool(_),
            Value::Arr(_) | Value::Obj(_) | Value::Func(_),
        ) => loose_eq(a, &Value::str(&to_str(b))),
        _ => strict_eq(a, b),
    }
}
