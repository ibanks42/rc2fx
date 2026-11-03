//! The saved RODE show is separate from RC2FX's processing configuration.
use crate::{CoreError, Result, valid_id};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StockSource {
    pub id: String,
    pub source_id: u16,
    /// None for a known source that is not assigned to a show fader.
    pub fader: Option<u8>,
    pub name: String,
    pub color: String,
    /// Empty when the installed route profile does not process this source.
    pub target_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StockInventory {
    pub available: bool,
    pub show_name: String,
    pub revision: String,
    pub message: String,
    pub inputs: Vec<StockSource>,
}

impl StockInventory {
    pub fn validate(&self) -> Result<()> {
        let text = |s: &str, max: usize| s.len() <= max && !s.chars().any(char::is_control);
        let hex = |s: &str, len| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !text(&self.show_name, 128) || !text(&self.message, 256) || self.inputs.len() > 32 {
            return Err(CoreError("Invalid show metadata".into()));
        }
        if !self.available {
            return if self.inputs.is_empty() && self.revision.is_empty() {
                Ok(())
            } else {
                Err(CoreError(
                    "Unavailable show contains input assignments".into(),
                ))
            };
        }
        let mut sources = HashSet::new();
        let mut faders = HashSet::new();
        let mut targets = HashSet::new();
        if !hex(&self.revision, 64)
            || self.inputs.iter().any(|s| {
                s.source_id > 255
                    || s.id != format!("input-{}", s.source_id)
                    || !sources.insert(s.source_id)
                    || s.fader
                        .is_some_and(|fader| !(1..=9).contains(&fader) || !faders.insert(fader))
                    || !text(&s.name, 128)
                    || s.name.trim().is_empty()
                    || !hex(&s.color, 8)
                    || (!s.target_id.is_empty()
                        && (!valid_id(&s.target_id) || !targets.insert(&s.target_id)))
            })
        {
            return Err(CoreError("Invalid or duplicate show input".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assignments_require_unique_physical_sources_and_processing_bindings() {
        let mut inventory = StockInventory {
            available: true,
            show_name: "Show".into(),
            revision: "a".repeat(64),
            message: String::new(),
            inputs: vec![StockSource {
                id: "input-3".into(),
                source_id: 3,
                fader: Some(1),
                name: "Mic 4".into(),
                color: "fffdba0e".into(),
                target_id: "mic-1".into(),
            }],
        };
        inventory.validate().unwrap();
        let mut other = inventory.inputs[0].clone();
        other.id = "input-7".into();
        other.source_id = 7;
        other.fader = Some(2);
        inventory.inputs.push(other);
        assert!(inventory.validate().is_err());
        inventory.inputs[1].target_id.clear();
        inventory.validate().unwrap();
        inventory.inputs[1].fader = Some(1);
        assert!(inventory.validate().is_err());
        inventory.inputs[0].fader = None;
        inventory.inputs[1].fader = None;
        inventory.validate().unwrap();
        let encoded = serde_json::to_vec(&inventory).unwrap();
        assert_eq!(
            serde_json::from_slice::<StockInventory>(&encoded).unwrap(),
            inventory
        );
        inventory.inputs[1].fader = Some(0);
        assert!(inventory.validate().is_err());
        inventory.inputs[1].fader = Some(10);
        assert!(inventory.validate().is_err());
        inventory.inputs.pop();
        inventory.available = false;
        assert!(inventory.validate().is_err());
    }
}
