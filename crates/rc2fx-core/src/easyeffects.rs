//! Supported EasyEffects IIR EQ interchange. This is data conversion only.
use crate::{Band, BandMode, BandType, Channel, CoreError, EqSettings, MAX_BANDS, Result};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const FREQUENCIES: [f64; 32] = [
    22.4, 27.8, 34.51, 42.82, 53.14, 65.95, 81.83, 101.55, 126.0, 156.38, 194.06, 240.81, 298.834,
    370.834, 460.182, 571.057, 708.647, 879.387, 1091.26, 1354.19, 1680.47, 2085.35, 2587.79,
    3211.29, 3985.01, 4945.15, 6136.63, 7615.17, 9449.96, 11726.8, 14552.2, 18058.4,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipeline {
    Input,
    Output,
}
impl Pipeline {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    pub stereo: bool,
    pub selected: Option<String>,
    pub channel: Option<Channel>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateSummary {
    pub key: String,
    pub pipeline: Pipeline,
    pub instance: String,
    pub band_count: usize,
    pub channels_differ: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ImportResult {
    Ready {
        eq: EqSettings,
        source: CandidateSummary,
        other_effects: usize,
    },
    ChooseEqualizer {
        candidates: Vec<CandidateSummary>,
    },
    ChooseChannel {
        source: CandidateSummary,
    },
}
#[derive(Clone, Debug)]
struct Equalizer {
    enabled: bool,
    input_gain: f64,
    output_gain: f64,
    left: Vec<Band>,
    right: Vec<Band>,
}
struct Candidate {
    summary: CandidateSummary,
    eq: Result<Equalizer>,
}
fn bad(message: impl Into<String>) -> CoreError {
    CoreError(message.into())
}
fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| bad(format!("{name} must be an object")))
}
fn number(map: &Map<String, Value>, key: &str, default: f64, min: f64, max: f64) -> Result<f64> {
    let value = match map.get(key) {
        Some(v) => v
            .as_f64()
            .ok_or_else(|| bad(format!("{key} must be a number")))?,
        None => default,
    };
    crate::model::range(value, min, max, key)?;
    Ok(value)
}
fn boolean(map: &Map<String, Value>, key: &str, default: bool) -> Result<bool> {
    match map.get(key) {
        Some(v) => v
            .as_bool()
            .ok_or_else(|| bad(format!("{key} must be true or false"))),
        None => Ok(default),
    }
}
fn token<'a>(map: &'a Map<String, Value>, key: &str, default: &'a str) -> Result<&'a str> {
    match map.get(key) {
        Some(v) => v.as_str().ok_or_else(|| bad(format!("{key} must be text"))),
        None => Ok(default),
    }
}
fn only_keys(map: &Map<String, Value>, allowed: &[&str], where_: &str) -> Result<()> {
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(bad(format!("Unsupported {where_} field: {key}")));
    }
    Ok(())
}
fn read_band(value: &Value, index: usize) -> Result<Band> {
    let map = object(value, "Band")?;
    only_keys(
        map,
        &[
            "type",
            "mode",
            "slope",
            "solo",
            "mute",
            "width",
            "frequency",
            "gain",
            "q",
        ],
        "band",
    )?;
    let band_type = match token(map, "type", "Bell")? {
        "Off" => BandType::Off,
        "Bell" => BandType::Bell,
        "Lo-shelf" => BandType::LowShelf,
        "Hi-shelf" => BandType::HighShelf,
        "Lo-pass" => BandType::LowPass,
        "Hi-pass" => BandType::HighPass,
        "Notch" => BandType::Notch,
        other => return Err(bad(format!("Unsupported filter type: {other}"))),
    };
    let mode = match token(map, "mode", "RLC (BT)")? {
        "APO (DR)" => BandMode::ApoDr,
        "RLC (BT)" => BandMode::RlcBt,
        other => return Err(bad(format!("Unsupported filter model: {other}"))),
    };
    // `slope` and `width` are accepted and ignored: RC2FX processes every band
    // as one filter section and none of its filters use a width, so a slope
    // above x1 sounds gentler here than in EasyEffects.
    let band = Band {
        id: format!("band-{}", index + 1),
        enabled: !boolean(map, "mute", false)?,
        band_type,
        mode,
        solo: boolean(map, "solo", false)?,
        frequency: number(map, "frequency", FREQUENCIES[index], 10.0, 24000.0)?,
        gain: number(map, "gain", 0.0, -36.0, 36.0)?,
        q: number(map, "q", 4.36, 0.0, 100.0)?,
    };
    band.validate()?;
    Ok(band)
}
fn read_equalizer(value: &Value) -> Result<Equalizer> {
    let map = object(value, "Equalizer")?;
    only_keys(
        map,
        &[
            "mode",
            "decramp",
            "balance",
            "pitch-left",
            "pitch-right",
            "bypass",
            "split-channels",
            "input-gain",
            "output-gain",
            "num-bands",
            "left",
            "right",
        ],
        "equalizer",
    )?;
    let mode = token(map, "mode", "IIR")?;
    if mode != "IIR" {
        return Err(bad(format!(
            "Unsupported processing mode: {mode}; only IIR is supported"
        )));
    }
    let decramp = token(map, "decramp", "Off")?;
    if decramp != "Off" {
        return Err(bad("Decramp must be Off"));
    }
    if number(map, "balance", 0.0, -100.0, 100.0)? != 0.0 {
        return Err(bad("Nonzero balance is unsupported"));
    }
    for key in ["pitch-left", "pitch-right"] {
        if number(map, key, 0.0, -120.0, 120.0)? != 0.0 {
            return Err(bad("Frequency pitch shifting is unsupported"));
        }
    }
    let enabled = !boolean(map, "bypass", false)?;
    boolean(map, "split-channels", false)?;
    let input_gain = number(map, "input-gain", 0.0, -36.0, 36.0)?;
    let output_gain = number(map, "output-gain", 0.0, -36.0, 36.0)?;
    let count = match map.get("num-bands") {
        Some(v) => v
            .as_u64()
            .filter(|v| (1..=32).contains(v))
            .ok_or_else(|| bad("num-bands must be an integer from 1 to 32"))?
            as usize,
        None => 32,
    };
    let mut sides = Vec::new();
    for side in ["left", "right"] {
        let channel = object(
            map.get(side)
                .ok_or_else(|| bad(format!("Missing {side} channel")))?,
            side,
        )?;
        if channel.keys().any(|k| {
            k.strip_prefix("band")
                .and_then(|s| s.parse::<usize>().ok())
                .is_none_or(|n| n >= MAX_BANDS)
        }) {
            return Err(bad("Unsupported channel field"));
        }
        let mut bands = Vec::new();
        for index in 0..count {
            bands.push(read_band(
                channel
                    .get(&format!("band{index}"))
                    .ok_or_else(|| bad(format!("Missing {side} band{index}")))?,
                index,
            )?)
        }
        sides.push(bands);
    }
    let right = sides.pop().unwrap();
    let left = sides.pop().unwrap();
    Ok(Equalizer {
        enabled,
        input_gain,
        output_gain,
        left,
        right,
    })
}
fn read_candidates(text: &str) -> Result<(Vec<Candidate>, usize)> {
    let root = crate::strict_json::parse(
        text.strip_prefix('\u{feff}').unwrap_or(text),
        MAX_FILE_BYTES,
    )?;
    let root = object(&root, "Preset")?;
    let mut candidates = Vec::new();
    let mut other_effects = 0;
    for pipeline in [Pipeline::Input, Pipeline::Output] {
        let Some(section) = root.get(pipeline.as_str()) else {
            continue;
        };
        let section = object(section, "Pipeline")?;
        let order = section
            .get("plugins_order")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("Pipeline requires a plugins_order array"))?;
        if let Some(blocklist) = section.get("blocklist")
            && !blocklist
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err(bad("blocklist must contain names"));
        }
        let mut seen = HashSet::new();
        let mut canonical = HashSet::new();
        for value in order {
            let entry = value
                .as_str()
                .ok_or_else(|| bad("plugins_order must contain names"))?;
            if !seen.insert(entry) {
                return Err(bad("plugins_order contains a duplicate effect"));
            }
            if entry.split('#').next() != Some("equalizer") {
                other_effects += 1;
                continue;
            }
            let instance = if entry == "equalizer" {
                "equalizer#0"
            } else {
                let suffix = entry
                    .strip_prefix("equalizer#")
                    .ok_or_else(|| bad("Invalid equalizer instance"))?;
                if suffix.is_empty()
                    || suffix.len() > 3
                    || !suffix.bytes().all(|c| c.is_ascii_digit())
                {
                    return Err(bad("Invalid equalizer instance"));
                }
                entry
            };
            if !canonical.insert(instance) {
                return Err(bad("Duplicate equalizer instance"));
            }
            let value = if entry == "equalizer" {
                section.get(instance).or_else(|| section.get(entry))
            } else {
                section.get(entry)
            };
            let eq = value
                .ok_or_else(|| bad("Equalizer settings are missing"))
                .and_then(read_equalizer);
            let summary = CandidateSummary {
                key: format!("{}/{instance}", pipeline.as_str()),
                pipeline,
                instance: instance.into(),
                band_count: eq.as_ref().map_or(0, |e| e.left.len()),
                channels_differ: eq.as_ref().is_ok_and(|e| e.left != e.right),
                error: eq.as_ref().err().map(ToString::to_string),
            };
            candidates.push(Candidate { summary, eq });
        }
    }
    if candidates.is_empty() {
        return Err(bad(
            "No active equalizer was found in this EasyEffects preset",
        ));
    }
    Ok((candidates, other_effects))
}
fn neutral() -> Band {
    Band {
        band_type: BandType::Off,
        ..Band::new("band-1")
    }
}
fn is_neutral(band: &Band) -> bool {
    let mut compare = band.clone();
    compare.id = "band-1".into();
    compare == neutral()
}
fn strip_padding(mut bands: Vec<Band>) -> Vec<Band> {
    while bands.last().is_some_and(is_neutral) {
        bands.pop();
    }
    bands
}

pub fn import(text: &str, options: &ImportOptions) -> Result<ImportResult> {
    let (mut candidates, other_effects) = read_candidates(text)?;
    let selected = if let Some(key) = &options.selected {
        candidates
            .iter()
            .position(|c| &c.summary.key == key)
            .ok_or_else(|| bad("Selected equalizer is absent from this file"))?
    } else if candidates.len() > 1 {
        if candidates.iter().all(|c| c.eq.is_err()) {
            return Err(candidates.remove(0).eq.unwrap_err());
        }
        return Ok(ImportResult::ChooseEqualizer {
            candidates: candidates.into_iter().map(|c| c.summary).collect(),
        });
    } else {
        0
    };
    let Candidate { summary, eq } = candidates.remove(selected);
    let candidate = eq?;
    let differs = candidate.left != candidate.right;
    if differs && !options.stereo && options.channel.is_none() {
        return Ok(ImportResult::ChooseChannel { source: summary });
    }
    let (bands, right_bands) = if !differs {
        let bands = if candidate.left.len() == 1 && is_neutral(&candidate.left[0]) {
            Vec::new()
        } else {
            candidate.left
        };
        (bands, None)
    } else if options.stereo {
        (
            strip_padding(candidate.left),
            Some(strip_padding(candidate.right)),
        )
    } else {
        (
            strip_padding(if options.channel == Some(Channel::Right) {
                candidate.right
            } else {
                candidate.left
            }),
            None,
        )
    };
    let eq = EqSettings {
        enabled: candidate.enabled,
        input_gain_db: candidate.input_gain,
        output_gain_db: candidate.output_gain,
        bands,
        right_bands,
    };
    eq.validate(if options.stereo { 2 } else { 1 })?;
    Ok(ImportResult::Ready {
        eq,
        source: summary,
        other_effects,
    })
}
fn type_name(value: BandType) -> &'static str {
    match value {
        BandType::Off => "Off",
        BandType::Bell => "Bell",
        BandType::LowShelf => "Lo-shelf",
        BandType::HighShelf => "Hi-shelf",
        BandType::LowPass => "Lo-pass",
        BandType::HighPass => "Hi-pass",
        BandType::Notch => "Notch",
    }
}
fn export_band(band: &Band) -> Value {
    json!({"type":type_name(band.band_type),"mode":if band.mode==BandMode::ApoDr{"APO (DR)"}else{"RLC (BT)"},"solo":band.solo,"mute":!band.enabled,"gain":band.gain,"frequency":band.frequency,"q":band.q})
}
pub fn export(eq: &EqSettings, pipeline: Pipeline) -> Result<String> {
    eq.validate(if eq.right_bands.is_some() { 2 } else { 1 })?;
    let right = eq.right_bands.as_deref().unwrap_or(&eq.bands);
    let count = eq.bands.len().max(right.len()).max(1);
    let placeholder = neutral();
    let channel = |bands: &[Band]| -> Value {
        Value::Object(
            (0..count)
                .map(|i| {
                    (
                        format!("band{i}"),
                        export_band(bands.get(i).unwrap_or(&placeholder)),
                    )
                })
                .collect(),
        )
    };
    let value = json!({pipeline.as_str():{"blocklist":[],"plugins_order":["equalizer#0"],"equalizer#0":{
        "bypass":!eq.enabled,"input-gain":eq.input_gain_db,"output-gain":eq.output_gain_db,"mode":"IIR","decramp":"Off","split-channels":eq.right_bands.is_some(),"balance":0,"pitch-left":0,"pitch-right":0,"num-bands":count,"left":channel(&eq.bands),"right":channel(right)}}});
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}
