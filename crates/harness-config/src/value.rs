//! A small format-agnostic value abstraction so the loader's untyped stages (interpolate,
//! merge) and the typed-dispatch stage (`build_run_config`) can run over either a
//! [`toml::Value`] (TOML input) or a [`serde_json::Value`] (JSON input) with one code path.
//!
//! Why this exists: TOML integers are a signed 64-bit type, but a scenario `op` amount can be a
//! `u64`/`u128` value in `(i64::MAX, u64::MAX]`. Routing JSON input through `toml::Value` (as the
//! loader once did) downgraded such integers to a lossy float, so a `.replay.json` sidecar failed
//! to round-trip. Processing JSON natively as `serde_json::Value` keeps integer precision (up to
//! `u64::MAX`; see the crate docs on `> u64::MAX`), while TOML input keeps its exact prior
//! behavior because `toml::Value` is still the value type on that path.
//!
//! The trait is deliberately minimal: only the operations the two untyped stages actually need.
//! The existing `toml::Value`-based unit tests in `interpolate` and `merge` exercise the generic
//! code with `V = toml::Value`, so the TOML path stays behavior-identical by construction.

use serde::de::DeserializeOwned;

/// A JSON/TOML-agnostic document value. Implemented for [`toml::Value`] and
/// [`serde_json::Value`]; both are also `serde` deserializers, which [`Doc::deserialize_into`]
/// relies on for the typed stage.
pub trait Doc: Clone + Sized + DeserializeOwned {
    /// The associated object/table map type (`toml::Table` or `serde_json::Map`).
    type Map: DocMap<Value = Self>;

    /// Returns a mutable reference to the inner `String` when this value is a string scalar,
    /// so interpolation can rewrite it in place.
    fn as_str_mut(&mut self) -> Option<&mut String>;
    /// Returns a mutable reference to the inner array when this value is an array.
    fn as_array_mut(&mut self) -> Option<&mut Vec<Self>>;
    /// Returns a mutable reference to the inner map when this value is a table/object.
    fn as_object_mut(&mut self) -> Option<&mut Self::Map>;
    /// Returns a shared reference to the inner map when this value is a table/object.
    fn as_object(&self) -> Option<&Self::Map>;
    /// Consumes the value, returning its inner map when it is a table/object.
    fn into_object(self) -> Option<Self::Map>;
    /// Whether this value is a table/object.
    fn is_object(&self) -> bool;
    /// Returns the string slice when this value is a string scalar.
    fn as_str(&self) -> Option<&str>;
    /// Wraps a map back into a value.
    fn from_object(map: Self::Map) -> Self;
    /// Deserializes this value into a typed `T`, stringifying the format-specific error. Both
    /// value types implement `serde::Deserializer`, so this preserves integer precision natively.
    fn deserialize_into<T: DeserializeOwned>(self) -> Result<T, String>;
}

/// The map operations the merge stage needs, over `toml::Table` / `serde_json::Map`.
pub trait DocMap: Clone {
    /// The value type stored in the map.
    type Value;

    /// A new, empty map.
    fn new() -> Self;
    /// Whether the map has no entries.
    fn is_empty(&self) -> bool;
    /// Whether `key` is present.
    fn contains_key(&self, key: &str) -> bool;
    /// A shared reference to the value at `key`.
    fn get(&self, key: &str) -> Option<&Self::Value>;
    /// A mutable reference to the value at `key`.
    fn get_mut(&mut self, key: &str) -> Option<&mut Self::Value>;
    /// Removes and returns the value at `key`.
    fn remove(&mut self, key: &str) -> Option<Self::Value>;
    /// Inserts `value` at `key`, returning any prior value.
    fn insert(&mut self, key: String, value: Self::Value) -> Option<Self::Value>;
    /// Iterates key/value pairs.
    fn iter(&self) -> impl Iterator<Item = (&String, &Self::Value)>;
    /// Iterates key/value pairs mutably.
    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Self::Value)>;
}

impl Doc for toml::Value {
    type Map = toml::value::Table;

    fn as_str_mut(&mut self) -> Option<&mut String> {
        match self {
            toml::Value::String(s) => Some(s),
            _ => None,
        }
    }
    fn as_array_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            toml::Value::Array(a) => Some(a),
            _ => None,
        }
    }
    fn as_object_mut(&mut self) -> Option<&mut Self::Map> {
        match self {
            toml::Value::Table(t) => Some(t),
            _ => None,
        }
    }
    fn as_object(&self) -> Option<&Self::Map> {
        match self {
            toml::Value::Table(t) => Some(t),
            _ => None,
        }
    }
    fn into_object(self) -> Option<Self::Map> {
        match self {
            toml::Value::Table(t) => Some(t),
            _ => None,
        }
    }
    fn is_object(&self) -> bool {
        matches!(self, toml::Value::Table(_))
    }
    fn as_str(&self) -> Option<&str> {
        toml::Value::as_str(self)
    }
    fn from_object(map: Self::Map) -> Self {
        toml::Value::Table(map)
    }
    fn deserialize_into<T: DeserializeOwned>(self) -> Result<T, String> {
        T::deserialize(self).map_err(|e| e.to_string())
    }
}

impl DocMap for toml::value::Table {
    type Value = toml::Value;

    fn new() -> Self {
        toml::value::Table::new()
    }
    fn is_empty(&self) -> bool {
        toml::value::Table::is_empty(self)
    }
    fn contains_key(&self, key: &str) -> bool {
        toml::value::Table::contains_key(self, key)
    }
    fn get(&self, key: &str) -> Option<&Self::Value> {
        toml::value::Table::get(self, key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut Self::Value> {
        toml::value::Table::get_mut(self, key)
    }
    fn remove(&mut self, key: &str) -> Option<Self::Value> {
        toml::value::Table::remove(self, key)
    }
    fn insert(&mut self, key: String, value: Self::Value) -> Option<Self::Value> {
        toml::value::Table::insert(self, key, value)
    }
    fn iter(&self) -> impl Iterator<Item = (&String, &Self::Value)> {
        toml::value::Table::iter(self)
    }
    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Self::Value)> {
        toml::value::Table::iter_mut(self)
    }
}

impl Doc for serde_json::Value {
    type Map = serde_json::Map<String, serde_json::Value>;

    fn as_str_mut(&mut self) -> Option<&mut String> {
        match self {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        }
    }
    fn as_array_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            serde_json::Value::Array(a) => Some(a),
            _ => None,
        }
    }
    fn as_object_mut(&mut self) -> Option<&mut Self::Map> {
        match self {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        }
    }
    fn as_object(&self) -> Option<&Self::Map> {
        match self {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        }
    }
    fn into_object(self) -> Option<Self::Map> {
        match self {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        }
    }
    fn is_object(&self) -> bool {
        self.is_object()
    }
    fn as_str(&self) -> Option<&str> {
        serde_json::Value::as_str(self)
    }
    fn from_object(map: Self::Map) -> Self {
        serde_json::Value::Object(map)
    }
    fn deserialize_into<T: DeserializeOwned>(self) -> Result<T, String> {
        T::deserialize(self).map_err(|e| e.to_string())
    }
}

impl DocMap for serde_json::Map<String, serde_json::Value> {
    type Value = serde_json::Value;

    fn new() -> Self {
        serde_json::Map::new()
    }
    fn is_empty(&self) -> bool {
        serde_json::Map::is_empty(self)
    }
    fn contains_key(&self, key: &str) -> bool {
        serde_json::Map::contains_key(self, key)
    }
    fn get(&self, key: &str) -> Option<&Self::Value> {
        serde_json::Map::get(self, key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut Self::Value> {
        serde_json::Map::get_mut(self, key)
    }
    fn remove(&mut self, key: &str) -> Option<Self::Value> {
        serde_json::Map::remove(self, key)
    }
    fn insert(&mut self, key: String, value: Self::Value) -> Option<Self::Value> {
        serde_json::Map::insert(self, key, value)
    }
    fn iter(&self) -> impl Iterator<Item = (&String, &Self::Value)> {
        serde_json::Map::iter(self)
    }
    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Self::Value)> {
        serde_json::Map::iter_mut(self)
    }
}
