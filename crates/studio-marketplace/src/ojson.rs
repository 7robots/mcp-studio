//! An order-preserving JSON value.
//!
//! Marketplace files are diffed by humans, so a rewrite must keep keys in the
//! order they were written. `serde_json::Value` sorts object keys unless the
//! crate-wide `preserve_order` feature is on, and turning that on would change
//! every other crate in the workspace, so this module carries its own small
//! value type. It serializes with `serde_json`'s pretty printer, which matches
//! the 2-space style of the files Studio generates.

use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn object() -> Self {
        Json::Object(Vec::new())
    }

    pub fn str(s: impl Into<String>) -> Self {
        Json::String(s.into())
    }

    pub fn strings<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Json::Array(items.into_iter().map(|s| Json::String(s.into())).collect())
    }

    /// Builder: append a key (replacing an existing one in place).
    pub fn with(mut self, key: &str, value: Json) -> Self {
        self.set(key, value);
        self
    }

    /// Builder: append a key only when `value` is `Some`.
    pub fn with_opt(self, key: &str, value: Option<Json>) -> Self {
        match value {
            Some(v) => self.with(key, v),
            None => self,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(kv) => kv.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Set a key, keeping its position when it already exists.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Object(kv) = self {
            match kv.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => kv.push((key.to_string(), value)),
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Pretty JSON, 2-space indent, non-ASCII kept, trailing newline.
    pub fn to_pretty(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("Json always serializes");
        s.push('\n');
        s
    }

    pub fn parse(text: &str) -> Result<Json, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Order-insensitive view, for comparisons and schema validation.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Json always converts")
    }
}

impl From<&serde_json::Value> for Json {
    fn from(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Json::Null,
            serde_json::Value::Bool(b) => Json::Bool(*b),
            serde_json::Value::Number(n) => Json::Number(n.clone()),
            serde_json::Value::String(s) => Json::String(s.clone()),
            serde_json::Value::Array(a) => Json::Array(a.iter().map(Json::from).collect()),
            serde_json::Value::Object(m) => {
                Json::Object(m.iter().map(|(k, v)| (k.clone(), Json::from(v))).collect())
            }
        }
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => s.serialize_unit(),
            Json::Bool(b) => s.serialize_bool(*b),
            Json::Number(n) => n.serialize(s),
            Json::String(v) => s.serialize_str(v),
            Json::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Json::Object(kv) => {
                let mut map = s.serialize_map(Some(kv.len()))?;
                for (k, v) in kv {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Json, D::Error> {
        Json::deserialize(d)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        serde_json::Number::from_f64(v)
            .map(Json::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::String(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element()? {
            out.push(v);
        }
        Ok(Json::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut out: Vec<(String, Json)> = Vec::new();
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            match out.iter_mut().find(|(ek, _)| *ek == k) {
                Some(slot) => slot.1 = v,
                None => out.push((k, v)),
            }
        }
        Ok(Json::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_key_order_and_unicode() {
        let text = "{\n  \"z\": 1,\n  \"a\": [\n    \"—\"\n  ],\n  \"e\": [],\n  \"o\": {}\n}\n";
        let j = Json::parse(text).unwrap();
        assert_eq!(j.to_pretty(), text);
    }

    #[test]
    fn set_keeps_position() {
        let mut j = Json::object()
            .with("a", Json::str("1"))
            .with("b", Json::str("2"));
        j.set("a", Json::str("x"));
        assert_eq!(j.to_pretty(), "{\n  \"a\": \"x\",\n  \"b\": \"2\"\n}\n");
    }
}
