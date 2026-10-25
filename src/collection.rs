//! The entries of a Map or a Set. They keep their insertion order, and a key is compared as SameValueZero: NaN is
//! equal to NaN, +0 to -0, a string to a string with the same text, and an object to itself.

use std::collections::HashMap;
use std::rc::Rc;

use crate::value::{char_value, JsStr, Value};

/// The entries of one Map or Set. A deleted entry leaves a hole, so the order of the others is kept. The holes are
/// removed once they make up most of the table.
#[derive(Clone)]
pub struct Collection {
    /// A Set holds its values as keys, so each of its entries has the same key and value.
    pub is_set: bool,
    entries: Vec<Option<(Value, Value)>>,
    index: HashMap<Key, usize>,
}

/// A key compared as SameValueZero: keys that are equal share one `Key`.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Undef,
    Null,
    Bool(bool),
    /// The bits of a number, with every NaN the same and -0 as +0.
    Num(u64),
    Text(Rc<JsStr>),
    /// The address of an object, array, function or native function. The entry keeps it alive.
    Ref(usize),
}

impl Key {
    fn of(v: &Value) -> Key {
        match v {
            Value::Undef => Key::Undef,
            Value::Null => Key::Null,
            Value::Bool(b) => Key::Bool(*b),
            Value::Num(n) if n.is_nan() => Key::Num(f64::NAN.to_bits()),
            Value::Num(n) if *n == 0.0 => Key::Num(0),
            Value::Num(n) => Key::Num(n.to_bits()),
            Value::Str(s) => Key::Text(Rc::clone(s)),
            Value::Obj(o) => Key::Ref(Rc::as_ptr(o) as *const () as usize),
            Value::Arr(a) => Key::Ref(Rc::as_ptr(a) as *const () as usize),
            Value::Func(c) => Key::Ref(Rc::as_ptr(c) as *const () as usize),
            Value::Native(d) => Key::Ref(*d as *const _ as usize),
            Value::Cell(c) => Key::Ref(Rc::as_ptr(c) as *const () as usize),
        }
    }
}

impl Collection {
    pub fn new(is_set: bool) -> Collection {
        Collection {
            is_set,
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        let i = *self.index.get(&Key::of(key))?;
        self.entries[i].as_ref().map(|(_, v)| v)
    }

    pub fn has(&self, key: &Value) -> bool {
        self.index.contains_key(&Key::of(key))
    }

    /// Sets the value of `key`. A new key goes after the others, and -0 is stored as +0.
    pub fn set(&mut self, key: Value, value: Value) {
        let key = match key {
            Value::Num(n) if n == 0.0 => Value::Num(0.0),
            key => key,
        };
        match self.index.get(&Key::of(&key)) {
            Some(&i) => {
                if let Some(entry) = &mut self.entries[i] {
                    entry.1 = value;
                }
            }
            None => {
                self.index.insert(Key::of(&key), self.entries.len());
                self.entries.push(Some((key, value)));
            }
        }
    }

    /// Removes the entry with `key`, and tells whether there was one.
    pub fn delete(&mut self, key: &Value) -> bool {
        let Some(i) = self.index.remove(&Key::of(key)) else {
            return false;
        };
        self.entries[i] = None;
        if self.entries.len() > 32 && self.index.len() * 2 < self.entries.len() {
            self.compact();
        }
        true
    }

    /// Removes the holes, and numbers the entries again.
    fn compact(&mut self) {
        self.entries.retain(Option::is_some);
        self.index.clear();
        for (i, (key, _)) in self.entries.iter().flatten().enumerate() {
            self.index.insert(Key::of(key), i);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// Copies of the entries, in insertion order.
    pub fn pairs(&self) -> Vec<(Value, Value)> {
        self.entries.iter().flatten().cloned().collect()
    }
}

/// The items that `for...of` and a spread visit: an array's items, a string's characters, a Map's [key, value]
/// entries, or a Set's values. None for a value that is not iterable.
pub fn items_of(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Arr(a) => Some(a.borrow().clone()),
        Value::Str(s) => Some(s.chars().map(char_value).collect()),
        Value::Obj(o) => {
            let props = o.borrow();
            let c = props.collection.as_deref()?;
            Some(
                c.pairs()
                    .into_iter()
                    .map(|(k, v)| if c.is_set { k } else { Value::arr(vec![k, v]) })
                    .collect(),
            )
        }
        _ => None,
    }
}
