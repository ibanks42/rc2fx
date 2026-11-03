//! Equalizer APO / AutoEQ parametric text. This parses data only: Include,
//! expressions, channel routing and other commands are never executed or ignored.
use crate::{Band, BandMode, BandType, CoreError, EqSettings, MAX_BANDS, Result};

fn error(line: usize, message: impl std::fmt::Display) -> CoreError {
    CoreError(format!("APO line {line}: {message}"))
}
fn number(text: &str, line: usize) -> Result<f64> {
    text.parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
        .ok_or_else(|| error(line, format!("Expected a finite number, found {text}")))
}
fn equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Import up to 32 filters, keeping order, enabled state and exact parameter
/// values. APO's preamp maps to EQ input gain and applies to both linked sides.
pub fn import(text: &str) -> Result<EqSettings> {
    if text.len() > crate::easyeffects::MAX_FILE_BYTES {
        return Err(CoreError("EQ preset exceeds 4 MiB".into()));
    }
    let mut eq = EqSettings::default();
    let mut found = false;
    for (index, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = index + 1;
        let raw = raw.split('#').next().unwrap_or("").trim();
        if raw.is_empty() {
            continue;
        }
        let (command, value) = raw
            .split_once(':')
            .ok_or_else(|| error(line, "Expected Preamp: or Filter: followed by parameters"))?;
        let command: Vec<_> = command.split_whitespace().collect();
        let words: Vec<_> = value.split_whitespace().collect();
        if command.len() == 1 && equal(command[0], "Preamp") {
            if words.len() != 2 || !equal(words[1], "dB") {
                return Err(error(line, "Expected Preamp: <gain> dB"));
            }
            eq.input_gain_db += number(words[0], line)?;
            found = true;
            continue;
        }
        if command.is_empty()
            || !equal(command[0], "Filter")
            || command.len() > 2
            || (command.len() == 2 && command[1].parse::<u32>().is_err())
        {
            return Err(error(
                line,
                "Only Preamp and parametric Filter commands are supported",
            ));
        }
        if eq.bands.len() == MAX_BANDS {
            return Err(error(line, "At most 32 filters are supported"));
        }
        let mut band = Band::new(format!("band-{}", eq.bands.len() + 1));
        band.mode = BandMode::ApoDr;
        band.enabled = match words.first() {
            Some(state) if equal(state, "ON") => true,
            Some(state) if equal(state, "OFF") => false,
            _ => return Err(error(line, "Filter must start with ON or OFF")),
        };
        // REW also exports unused filters as just "Filter n: OFF".
        if !band.enabled && words.len() == 1 {
            band.band_type = BandType::Off;
            eq.bands.push(band);
            found = true;
            continue;
        }
        let kind = words
            .get(1)
            .ok_or_else(|| error(line, "Missing filter type"))?
            .to_ascii_uppercase();
        band.band_type = match kind.as_str() {
            "PK" | "PEQ" => BandType::Bell,
            "LSC" | "LS" => BandType::LowShelf,
            "HSC" | "HS" => BandType::HighShelf,
            "HP" | "HPQ" => BandType::HighPass,
            "LP" | "LPQ" => BandType::LowPass,
            "NO" => BandType::Notch,
            _ => return Err(error(line, format!("Unsupported filter type {kind}"))),
        };
        let (mut frequency, mut gain, mut q) = (None, None, None);
        let mut offset = 2;
        while offset < words.len() {
            let field = words[offset];
            if equal(field, "Fc") || equal(field, "Gain") {
                let v = words
                    .get(offset + 1)
                    .ok_or_else(|| error(line, "Missing parameter value"))?;
                let unit = words
                    .get(offset + 2)
                    .ok_or_else(|| error(line, "Missing Hz or dB unit"))?;
                let (slot, expected) = if equal(field, "Fc") {
                    (&mut frequency, "Hz")
                } else {
                    (&mut gain, "dB")
                };
                if slot.is_some() || !equal(unit, expected) {
                    return Err(error(
                        line,
                        format!("Repeated {field} or invalid unit (expected {expected})"),
                    ));
                }
                *slot = Some(number(v, line)?);
                offset += 3;
            } else if equal(field, "Q") {
                if q.is_some() {
                    return Err(error(line, "Repeated Q parameter"));
                }
                q = Some(number(
                    words
                        .get(offset + 1)
                        .ok_or_else(|| error(line, "Missing Q value"))?,
                    line,
                )?);
                offset += 2;
            } else {
                return Err(error(
                    line,
                    format!(
                        "Unsupported parameter {field}; use Fc, Gain and Q (slope/BW variants are not supported)"
                    ),
                ));
            }
        }
        band.frequency = frequency.ok_or_else(|| error(line, "Missing Fc frequency"))?;
        let has_gain = matches!(
            band.band_type,
            BandType::Bell | BandType::LowShelf | BandType::HighShelf
        );
        band.gain = if has_gain {
            gain.ok_or_else(|| error(line, "Missing Gain"))?
        } else if gain.is_some() {
            return Err(error(line, "This filter type does not accept Gain"));
        } else {
            0.0
        };
        let needs_q = matches!(kind.as_str(), "PK" | "PEQ" | "HPQ" | "LPQ" | "NO");
        band.q = if needs_q {
            q.ok_or_else(|| error(line, "Missing Q"))?
        } else {
            q.unwrap_or(std::f64::consts::FRAC_1_SQRT_2)
        };
        // The engine clamps Q below .1 and frequencies above .49 * fs. Reject
        // such imports explicitly rather than silently changing their response.
        crate::model::range(band.q, 0.1, 100.0, "Q").map_err(|e| error(line, e))?;
        crate::model::range(band.frequency, 10.0, crate::SAMPLE_RATE * 0.49, "Frequency")
            .map_err(|e| error(line, e))?;
        band.validate().map_err(|e| error(line, e))?;
        eq.bands.push(band);
        found = true;
    }
    if !found {
        return Err(CoreError("No APO preamp or filters found".into()));
    }
    eq.validate(2)?;
    Ok(eq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Channel,
        easyeffects::{self, ImportOptions, ImportResult, Pipeline},
    };
    #[test]
    fn supplied_autoeq_and_fifteen_band_presets_preserve_their_curves() {
        for (text, count, preamp) in [
            (
                include_str!("../../../fixtures/apo/sennheiser-hd-6xx.txt"),
                10,
                -8.46,
            ),
            (
                include_str!("../../../fixtures/apo/fifteen-band.txt"),
                15,
                -15.5,
            ),
        ] {
            let eq = import(text).unwrap();
            assert_eq!(eq.bands.len(), count);
            assert_eq!(eq.input_gain_db, preamp);
            assert_eq!(eq.output_gain_db, 0.);
            assert!(eq.right_bands.is_none());
            assert!(
                eq.bands
                    .iter()
                    .all(|b| b.enabled && b.mode == BandMode::ApoDr)
            );
            assert_eq!(eq.bands[0].band_type, BandType::LowShelf);
            let shelf = &eq.bands[0];
            let center =
                crate::eq::magnitude_db(crate::eq::coefficients(shelf).unwrap(), shelf.frequency);
            assert!((center - shelf.gain / 2.).abs() < 1e-5);
            let json = easyeffects::export(&eq, Pipeline::Output).unwrap();
            let ImportResult::Ready { eq: restored, .. } = easyeffects::import(
                &json,
                &ImportOptions {
                    stereo: true,
                    ..Default::default()
                },
            )
            .unwrap() else {
                panic!()
            };
            for hz in [20., 75., 105., 1000., 10000., 20000.] {
                assert!(
                    (crate::eq::response_db(&eq, Channel::Left, hz, true)
                        - crate::eq::response_db(&restored, Channel::Right, hz, true))
                    .abs()
                        < 1e-9
                );
            }
        }
        let eq = import(include_str!("../../../fixtures/apo/fifteen-band.txt")).unwrap();
        assert_eq!(
            (eq.bands[0].frequency, eq.bands[0].gain, eq.bands[0].q),
            (21., 15.31, 0.475)
        );
        assert_eq!(eq.bands[12].band_type, BandType::HighShelf);
        assert_eq!((eq.bands[14].frequency, eq.bands[14].q), (12178., 0.643));
    }
    #[test]
    fn comments_numbering_disabled_filters_and_additive_preamp() {
        let eq = import("\u{feff}# REW\r\nPreamp: -2 dB\nPreamp: -1 dB\nFilter 99: OFF PK Fc 500 Hz Gain 3 dB Q 1 # retained\nFilter: ON HP Fc 40 Hz\nFilter 99: OFF").unwrap();
        assert_eq!(eq.input_gain_db, -3.);
        assert_eq!(eq.bands.len(), 3);
        assert!(!eq.bands[0].enabled);
        assert_eq!(eq.bands[0].gain, 3.);
        assert_eq!(eq.bands[1].band_type, BandType::HighPass);
        assert_eq!(eq.bands[2].band_type, BandType::Off);
    }
    #[test]
    fn rejects_incomplete_unsupported_and_silently_clamped_settings() {
        for text in [
            "",
            "Include: other.txt",
            "Channel: L",
            "Preamp: NaN dB",
            "Preamp: -100 dB",
            "Filter: ON PK Fc 50 Hz Gain 2 dB",
            "Filter: ON PK Fc 50 Hz Gain 2 dB Q 0.01",
            "Filter: ON PK Fc 24000 Hz Gain 2 dB Q 1",
            "Filter: ON LSC 6 dB Fc 100 Hz Gain 2 dB",
            "Filter: ON PK Fc 50 Hz Fc 60 Hz Gain 1 dB Q 1",
        ] {
            assert!(import(text).is_err(), "{text}");
        }
        assert!(import(&"Filter: ON HP Fc 40 Hz\n".repeat(33)).is_err());
    }
}
