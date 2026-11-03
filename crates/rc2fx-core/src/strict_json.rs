use crate::{CoreError, Result};
use serde::Deserializer;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::collections::HashSet;

/// Validate without retaining scalar/array values. Object keys remain live only
/// for the enclosing object so escaped duplicate spellings are still rejected.
pub fn validate(text: &str, maximum_bytes: usize, maximum_nodes: usize) -> Result<()> {
    if text.len() > maximum_bytes {
        return Err(CoreError("JSON file is too large".into()));
    }
    let mut decoder = serde_json::Deserializer::from_str(text);
    let mut nodes = 0;
    CheckSeed {
        depth: 0,
        nodes: &mut nodes,
        maximum_nodes,
    }
    .deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(())
}

struct CheckSeed<'a> {
    depth: usize,
    nodes: &'a mut usize,
    maximum_nodes: usize,
}
impl<'de> DeserializeSeed<'de> for CheckSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, decoder: D) -> std::result::Result<(), D::Error> {
        *self.nodes += 1;
        if self.depth > 64 || *self.nodes > self.maximum_nodes {
            return Err(D::Error::custom(
                "JSON is too deeply nested or contains too many values",
            ));
        }
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for CheckSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON value")
    }
    fn visit_bool<E: Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: Error>(self, value: f64) -> std::result::Result<(), E> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(E::custom("Number must be finite"))
        }
    }
    fn visit_str<E: Error>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_string<E: Error>(self, _: String) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<(), A::Error> {
        while sequence
            .next_element_seed(CheckSeed {
                depth: self.depth + 1,
                nodes: self.nodes,
                maximum_nodes: self.maximum_nodes,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(A::Error::custom("Duplicate JSON key"));
            }
            map.next_value_seed(CheckSeed {
                depth: self.depth + 1,
                nodes: self.nodes,
                maximum_nodes: self.maximum_nodes,
            })?;
        }
        Ok(())
    }
}

pub fn parse(text: &str, maximum_bytes: usize) -> Result<Value> {
    parse_with_limits(text, maximum_bytes, 200_000)
}

pub fn parse_with_limits(text: &str, maximum_bytes: usize, maximum_nodes: usize) -> Result<Value> {
    if text.len() > maximum_bytes {
        return Err(CoreError("JSON file is too large".into()));
    }
    let mut decoder = serde_json::Deserializer::from_str(text);
    let mut nodes = 0;
    let value = Seed {
        depth: 0,
        nodes: &mut nodes,
        maximum_nodes,
    }
    .deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(value)
}
struct Seed<'a> {
    depth: usize,
    nodes: &'a mut usize,
    maximum_nodes: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, decoder: D) -> std::result::Result<Value, D::Error> {
        *self.nodes += 1;
        if self.depth > 64 || *self.nodes > self.maximum_nodes {
            return Err(D::Error::custom(
                "JSON is too deeply nested or contains too many values",
            ));
        }
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON value")
    }
    fn visit_bool<E: Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: Error>(self, value: f64) -> std::result::Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("Number must be finite"))
    }
    fn visit_str<E: Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_string<E: Error>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E: Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Seed {
            depth: self.depth + 1,
            nodes: self.nodes,
            maximum_nodes: self.maximum_nodes,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom(format!("Duplicate JSON key: {key}")));
            }
            let value = map.next_value_seed(Seed {
                depth: self.depth + 1,
                nodes: self.nodes,
                maximum_nodes: self.maximum_nodes,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
