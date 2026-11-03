//! Read-only projection of the stock JUCE ValueTree. Unrelated properties may
//! contain account/network information: neither values nor parse errors escape.
//! This runs on a bounded control task, never an audio callback.
use rc2fx_core::{Config, StockInventory, StockSource};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read},
    path::Path,
};

const MAX_BYTES: usize = 2 << 20;
/// Same borrowed shape as the qualified routing catalog: target and stock
/// source/sink followed by processor input/output. No registration is performed.
pub type CatalogChannel<'a> = (&'a str, [&'a str; 4]);

pub fn unavailable(message: &str) -> StockInventory {
    StockInventory {
        available: false,
        show_name: String::new(),
        revision: String::new(),
        message: message
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect(),
        inputs: vec![],
    }
}

/// The path is fixed by local startup configuration, never a request argument.
/// Each call reads anew; a partial save cannot leave a cached layout on screen.
pub fn read(path: &Path, config: &Config, catalog: &[CatalogChannel<'_>]) -> StockInventory {
    // The board-checked daemon supplies this catalog, not the saved show.
    let model = if catalog.iter().any(|(id, _)| id.starts_with("duo-")) {
        rc2fx_firmware::Model::Duo
    } else {
        rc2fx_firmware::Model::ProIi
    };
    let result = (|| -> io::Result<_> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        read_file(file, path, model)
    })();
    let Ok(mut inventory) = result else {
        return unavailable("The mixer's saved show is unavailable or being saved; retry shortly");
    };
    for source in &mut inventory.inputs {
        source.target_id = binding(source.source_id, config, catalog).unwrap_or_default();
    }
    if inventory.validate().is_err() {
        return unavailable("The saved show has ambiguous processing bindings");
    }
    inventory
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    let common = a.is_file()
        && b.is_file()
        && a.len() == b.len()
        && matches!((a.modified(), b.modified()), (Ok(x), Ok(y)) if x == y);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        common
            && a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        common
    }
}

fn read_file(
    mut file: File,
    path: &Path,
    model: rc2fx_firmware::Model,
) -> io::Result<StockInventory> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_BYTES as u64 {
        return Err(invalid());
    }
    let mut data = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut data)?;
    if data.len() > MAX_BYTES
        || data.len() as u64 != before.len()
        || !same_file(&before, &file.metadata()?)
        || !same_file(&before, &fs::symlink_metadata(path)?)
    {
        return Err(invalid());
    }
    decode_for_model(&data, model)
}

fn name(id: u16, model: rc2fx_firmware::Model) -> Option<String> {
    if model == rc2fx_firmware::Model::Duo && id == 2 {
        return Some("Headset mic".into());
    }
    Some(match id {
        0..=3 => format!("Mic {}", id + 1),
        4..=6 => format!("Mic {} + {}", id - 3, id - 2),
        7 => "USB 1".into(),
        8 => "Chat".into(),
        9 => "USB 2".into(),
        10 => "Bluetooth".into(),
        11 => "SMART Pads".into(),
        // Vendor source-name switch, not the user's fader labels. Naming a
        // source does not admit it as an independent processing route.
        // See docs/reference/source-name-evidence.md and the offline audit.
        12 => "Game".into(),
        13 => "Music".into(),
        14 => "Virtual A".into(),
        15 => "Virtual B".into(),
        16..=18 => format!("CallMe {}", id - 15),
        19..=20 => format!("Wireless {}", id - 18),
        21..=29 => format!("RCV Channel {}", id - 20),
        _ => return None,
    })
}

fn binding(source: u16, config: &Config, catalog: &[CatalogChannel<'_>]) -> Option<String> {
    let pairs: Vec<(String, String)> = match source {
        0..=3 => vec![(
            format!("system:capture_{}", source + 1),
            format!("JUCEJack:in_{}", source + 1),
        )],
        7 => vec![
            ("usb1in:capture_1".into(), "JUCEJack:in_7".into()),
            ("usb1in:capture_2".into(), "JUCEJack:in_8".into()),
        ],
        8 => vec![
            ("usb1-chat-in:capture_1".into(), "JUCEJack:in_11".into()),
            ("usb1-chat-in:capture_2".into(), "JUCEJack:in_12".into()),
        ],
        9 => vec![
            ("usb2in:capture_1".into(), "JUCEJack:in_9".into()),
            ("usb2in:capture_2".into(), "JUCEJack:in_10".into()),
        ],
        10 => vec![
            ("btphone:capture_1".into(), "JUCEJack:in_5".into()),
            ("btphone:capture_2".into(), "JUCEJack:in_6".into()),
        ],
        12..=15 => {
            // Verified virtual-device source slots 22..29 are fed from the
            // first four stereo pairs after USB Main in the expanded input.
            let first = (source - 12) * 2 + 3;
            (first..=first + 1)
                .map(|channel| {
                    (
                        format!("usb1in:capture_{channel}"),
                        format!("JUCEJack:in_{}", channel + 10),
                    )
                })
                .collect()
        }
        16..=18 => {
            let first = (source - 16) * 2 + 1;
            (first..=first + 1)
                .map(|channel| {
                    (
                        format!("sip-audio-in:capture_{channel}"),
                        format!("JUCEJack:in_{}", channel + 28),
                    )
                })
                .collect()
        }
        19..=20 => vec![(
            format!("RodeRfSPI:TX{}", source - 18),
            format!("JUCEJack:in_{}", source + 16),
        )],
        _ => return None,
    };
    let mut matches = config.targets.iter().filter(|target| {
        let expected_kind = if source < 4 || (19..=20).contains(&source) {
            rc2fx_core::TargetKind::Mic
        } else {
            rc2fx_core::TargetKind::Playback
        };
        let routes: Vec<_> = catalog.iter().filter(|(id, _)| *id == target.id).collect();
        target.kind == expected_kind
            && usize::from(target.channels) == pairs.len()
            && routes.len() == pairs.len()
            && routes
                .iter()
                .zip(&pairs)
                .all(|((_, ports), (src, dst))| ports[0] == src && ports[1] == dst)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first.id.clone())
}

/// The physical processing targets still used by assigned show sources, in
/// saved-config order. This is for removal detection, not new route admission.
/// Linked mics cover both existing mono paths; RCV aliases cover an existing
/// USB path, never a second chain. Unavailable or invalid reads give no answer.
/// Recompute bindings from the trusted local catalog, not inventory target IDs.
pub fn assigned_targets(
    inventory: &StockInventory,
    config: &Config,
    catalog: &[CatalogChannel<'_>],
) -> Option<Vec<String>> {
    if !inventory.available || inventory.validate().is_err() {
        return None;
    }
    let mut assigned = HashSet::new();
    for source in inventory
        .inputs
        .iter()
        .filter(|source| source.fader.is_some())
    {
        let ids: Vec<u16> = match source.source_id {
            4..=6 => vec![source.source_id - 4, source.source_id - 3],
            21 => vec![7],
            22..=25 => vec![source.source_id - 10],
            id => vec![id],
        };
        for id in ids {
            if let Some(target) = binding(id, config, catalog) {
                assigned.insert(target);
            }
        }
    }
    Some(
        config
            .targets
            .iter()
            .filter(|target| assigned.contains(&target.id))
            .map(|target| target.id.clone())
            .collect(),
    )
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Unsupported or incomplete saved show",
    )
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
    nodes: usize,
    channels: Vec<i32>,
    colors: HashMap<i32, &'a str>,
    show: Option<&'a str>,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.offset.checked_add(n).ok_or_else(invalid)?;
        let bytes = self.data.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        Ok(bytes)
    }
    fn count(&mut self) -> io::Result<usize> {
        let n = usize::from(self.take(1)?[0]);
        if n > 4 {
            return Err(invalid());
        }
        let mut value = 0_u32;
        for (i, b) in self.take(n)?.iter().enumerate() {
            value |= u32::from(*b) << (i * 8);
        }
        if value as usize > MAX_BYTES {
            return Err(invalid());
        }
        Ok(value as usize)
    }
    fn text(&mut self) -> io::Result<&'a str> {
        let n = self.data[self.offset..]
            .iter()
            .take(16385)
            .position(|b| *b == 0)
            .ok_or_else(invalid)?;
        let bytes = self.take(n + 1)?;
        std::str::from_utf8(&bytes[..n]).map_err(|_| invalid())
    }
    fn node(&mut self, depth: usize) -> io::Result<()> {
        self.nodes += 1;
        if depth > 32 || self.nodes > 16384 {
            return Err(invalid());
        }
        let name = self.text()?;
        if depth == 0 && name != "Rodecaster" {
            return Err(invalid());
        }
        let count = self.count()?;
        if count > 4096 {
            return Err(invalid());
        }
        let mut seen = HashSet::new();
        let mut source = None;
        let mut color = "";
        for _ in 0..count {
            let key = self.text()?;
            if !seen.insert(key) {
                return Err(invalid());
            }
            let size = self.count()?;
            let value = self.take(size)?;
            if depth != 1 {
                continue;
            }
            if matches!(
                (name, key),
                ("CHANNEL", "channelInputSource") | ("INPUTSOURCE", "inputId")
            ) {
                if value.len() != 5 || value[0] != 1 {
                    return Err(invalid());
                }
                let id = i32::from_le_bytes(value[1..].try_into().map_err(|_| invalid())?);
                if !(-1..=255).contains(&id) {
                    return Err(invalid());
                }
                source = Some(id);
            }
            if matches!(
                (name, key),
                ("INPUTSOURCE", "inputColor") | ("CURRENTSHOW", "currentShowName")
            ) {
                if value.len() < 2 || value[0] != 5 || value.last() != Some(&0) {
                    return Err(invalid());
                }
                let text =
                    std::str::from_utf8(&value[1..value.len() - 1]).map_err(|_| invalid())?;
                if text.len() > 128 || text.chars().any(char::is_control) {
                    return Err(invalid());
                }
                if name == "INPUTSOURCE" {
                    color = text;
                } else if self.show.replace(text).is_some() {
                    return Err(invalid());
                }
            }
        }
        if depth == 1 && name == "CHANNEL" {
            self.channels.push(source.ok_or_else(invalid)?);
        }
        if depth == 1 && name == "INPUTSOURCE" {
            let id = source.filter(|id| *id >= 0).ok_or_else(invalid)?;
            if self.colors.insert(id, color).is_some() {
                return Err(invalid());
            }
        }
        let count = self.count()?;
        if count > 4096 {
            return Err(invalid());
        }
        for _ in 0..count {
            self.node(depth + 1)?;
        }
        Ok(())
    }
}

/// Decode only public audio/show metadata. Unknown properties are skipped, not
/// retained. The complete tree must be well-formed before any result is returned.
pub fn decode(data: &[u8]) -> io::Result<StockInventory> {
    decode_for_model(data, rc2fx_firmware::Model::ProIi)
}

pub fn decode_for_model(data: &[u8], model: rc2fx_firmware::Model) -> io::Result<StockInventory> {
    if data.is_empty() || data.len() > MAX_BYTES {
        return Err(invalid());
    }
    let mut reader = Reader {
        data,
        offset: 0,
        nodes: 0,
        channels: vec![],
        colors: HashMap::new(),
        show: None,
    };
    reader.node(0)?;
    let faders = if model == rc2fx_firmware::Model::Duo {
        7
    } else {
        9
    };
    if reader.offset != data.len() || reader.channels.len() < faders {
        return Err(invalid());
    }
    let mut result = StockInventory {
        available: true,
        show_name: reader.show.unwrap_or_default().into(),
        revision: format!("{:x}", Sha256::digest(data)),
        message: "RODE sources and saved fader assignments; connection availability is separate"
            .into(),
        inputs: vec![],
    };
    let mut seen = HashSet::new();
    for (fader, id) in reader.channels[..faders].iter().copied().enumerate() {
        if id < 0 {
            continue;
        }
        if !seen.insert(id) {
            return Err(invalid());
        }
        let color = reader.colors.get(&id).ok_or_else(invalid)?;
        let color = if color.len() == 8 && color.bytes().all(|b| b.is_ascii_hexdigit()) {
            *color
        } else {
            "ff929eaf"
        };
        result.inputs.push(StockSource {
            id: format!("input-{id}"),
            source_id: id as u16,
            fader: Some((fader + 1) as u8),
            name: name(id as u16, model).unwrap_or_else(|| format!("Fader {}", fader + 1)),
            color: color.into(),
            target_id: String::new(),
        });
    }
    // Include proven physical and virtual source types with an INPUTSOURCE
    // record, even when unassigned. RCV aliases 21..29 remain fader-only:
    // declaring an alias is not independent availability or another DSP chain.
    // Assigned sources above retain fader order, including unfamiliar IDs.
    for id in 0..=20 {
        if seen.contains(&id) || (model == rc2fx_firmware::Model::Duo && [3, 5, 6].contains(&id)) {
            continue;
        }
        let Some(color) = reader.colors.get(&id) else {
            continue;
        };
        let color = if color.len() == 8 && color.bytes().all(|b| b.is_ascii_hexdigit()) {
            *color
        } else {
            "ff929eaf"
        };
        let Some(name) = name(id as u16, model) else {
            continue;
        };
        result.inputs.push(StockSource {
            id: format!("input-{id}"),
            source_id: id as u16,
            fader: None,
            name,
            color: color.into(),
            target_id: String::new(),
        });
    }
    result.validate().map_err(|_| invalid())?;
    Ok(result)
}
