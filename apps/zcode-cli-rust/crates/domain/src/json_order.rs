//! Order-preserving JSON for rewriting user files. Node `JSON.parse` and
//! `JSON.stringify(value, null, 2)` keep key order; `serde_json::Value` here
//! sorts keys, so edits of user files go through this tree instead.
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Number, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Ordered {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Ordered>),
    Object(Vec<(String, Ordered)>),
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Tree;
        impl<'de> Visitor<'de> for Tree {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Ordered, E> {
                Ok(Ordered::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Ordered, E> {
                Ok(Ordered::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Ordered, E> {
                Ok(Ordered::Number(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Ordered, E> {
                Ok(Ordered::Number(v.into()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Ordered, E> {
                Ok(Number::from_f64(v).map_or(Ordered::Null, Ordered::Number))
            }
            fn visit_str<E>(self, v: &str) -> Result<Ordered, E> {
                Ok(Ordered::String(v.into()))
            }
            fn visit_string<E>(self, v: String) -> Result<Ordered, E> {
                Ok(Ordered::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Ordered, A::Error> {
                let mut items = vec![];
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Ordered::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut object = Ordered::Object(vec![]);
                while let Some((key, value)) = map.next_entry::<String, Ordered>()? {
                    // JSON.parse：重复键取最后的值，位置保持首次出现处。
                    object.set(&key, value);
                }
                Ok(object)
            }
        }
        deserializer.deserialize_any(Tree)
    }
}

impl Ordered {
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Ordered> {
        match self {
            Self::Object(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn at_mut(&mut self, index: usize) -> Option<&mut Ordered> {
        match self {
            Self::Array(items) => items.get_mut(index),
            _ => None,
        }
    }
    /// Replaces a key in place or appends it (JS property assignment).
    pub fn set(&mut self, key: &str, value: Ordered) {
        if let Self::Object(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key.into(), value)),
            }
        }
    }
    pub fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(*b),
            Self::Number(n) => Value::Number(n.clone()),
            Self::String(s) => Value::String(s.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_value).collect()),
            Self::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }
    /// `JSON.stringify(value, null, 2)`.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }
    fn write(&self, out: &mut String, depth: usize) {
        let indent = |out: &mut String, depth: usize| {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        };
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Number(n) => out.push_str(&match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => i.to_string(),
                (_, Some(u)) => u.to_string(),
                _ => crate::hooks::digest::js_number(n.as_f64().unwrap_or(0.0)),
            }),
            Self::String(s) => out.push_str(&Value::from(s.as_str()).to_string()),
            Self::Array(items) if items.is_empty() => out.push_str("[]"),
            Self::Object(entries) if entries.is_empty() => out.push_str("{}"),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    item.write(out, depth + 1);
                }
                indent(out, depth);
                out.push(']');
            }
            Self::Object(entries) => {
                out.push('{');
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    out.push_str(&Value::from(key.as_str()).to_string());
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_key_order_and_prints_like_json_stringify() {
        let text = r#"{"z":1,"a":{"y":[1,2.5,{}],"b":[]},"n":1.0,"s":"q\"\u0001é","z":2}"#;
        let mut tree: Ordered = serde_json::from_str(text).unwrap();
        tree.get_mut("a")
            .unwrap()
            .set("enabled", Ordered::Bool(false));
        assert_eq!(
            tree.pretty(),
            "{\n  \"z\": 2,\n  \"a\": {\n    \"y\": [\n      1,\n      2.5,\n      {}\n    ],\n    \"b\": [],\n    \"enabled\": false\n  },\n  \"n\": 1,\n  \"s\": \"q\\\"\\u0001é\"\n}"
        );
        assert_eq!(tree.to_value()["a"]["y"][1], 2.5);
    }
}
