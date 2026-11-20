use crate::{CoreError, Result};
use serde::Deserializer;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use std::collections::HashSet;

/// Validates JSON limits and duplicate keys without retaining values.
/// Keys are tracked per object so escaped duplicates are still caught.
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
