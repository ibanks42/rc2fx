//! Fixed RODECaster Pro II 1.7.6 path identities. Each channel is
//! `[stock source, mixer buffer]`; the native library binds the mixer buffer
//! and show discovery matches both.
use crate::stock_inputs::CatalogChannel;
use rc2fx_firmware::Model;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalKind {
    Mic,
    Playback,
    Output,
}
#[derive(Clone, Copy)]
pub struct PhysicalPath {
    pub id: &'static str,
    pub kind: PhysicalKind,
    pub channels: &'static [[&'static str; 2]],
}

pub fn physical_paths_for(model: Model) -> &'static [PhysicalPath] {
    match model {
        Model::ProIi => &PATHS,
        Model::Duo => &DUO_PATHS,
    }
}

/// Duo IDs are deliberately distinct: a Pro II preset must not silently move
/// a chain to the Duo's differently wired headphone or headset socket.
pub fn default_config(model: Model) -> rc2fx_core::Config {
    if model == Model::ProIi {
        return rc2fx_core::Config::default();
    }
    use rc2fx_core::{Config, Target, TargetKind};
    Config {
        targets: DUO_PATHS
            .iter()
            .map(|p| {
                let label = match p.id {
                    "duo-analog-1" => "Mic 1".into(),
                    "duo-analog-2" => "Mic 2".into(),
                    "duo-headset-mic" => "Headset mic".into(),
                    "duo-headset" => "Headset".into(),
                    "duo-usb-main" => "USB 1".into(),
                    "duo-usb-chat" => "Chat".into(),
                    "duo-usb-secondary" => "USB 2".into(),
                    "duo-usb-game" => "Game".into(),
                    "duo-usb-music" => "Music".into(),
                    "duo-usb-a" => "Virtual A".into(),
                    "duo-usb-b" => "Virtual B".into(),
                    "duo-monitors" => "Monitors".into(),
                    "duo-send-main" => "Main Mix Out".into(),
                    "duo-send-chat" => "Chat Mix Out".into(),
                    "duo-send-secondary" => "Secondary Mix Out".into(),
                    _ => p.id.trim_start_matches("duo-").replace('-', " "),
                };
                let kind = match p.kind {
                    PhysicalKind::Mic => TargetKind::Mic,
                    PhysicalKind::Playback => TargetKind::Playback,
                    PhysicalKind::Output => TargetKind::Output,
                };
                Target::new(p.id, label, kind, p.channels.len() as u8)
            })
            .collect(),
        ..Config::default()
    }
}
/// Flattened channel view for show discovery.
pub fn physical_catalog_for(model: Model) -> &'static [CatalogChannel<'static>] {
    static PRO: OnceLock<Vec<CatalogChannel<'static>>> = OnceLock::new();
    static DUO: OnceLock<Vec<CatalogChannel<'static>>> = OnceLock::new();
    let channels = match model {
        Model::ProIi => &PRO,
        Model::Duo => &DUO,
    };
    channels.get_or_init(|| {
        physical_paths_for(model)
            .iter()
            .flat_map(|p| p.channels.iter().map(|c| (p.id, *c)))
            .collect()
    })
}
/// The mixer buffer name the native library binds, without the client prefix.
pub fn mixer_port(path: &PhysicalPath, channel: &[&'static str; 2]) -> &'static str {
    let port = if path.kind == PhysicalKind::Output {
        channel[0]
    } else {
        channel[1]
    };
    port.strip_prefix("JUCEJack:").unwrap_or(port)
}

/// Bluetooth music mode (A2DP). Phone mode reuses the same mixer buffers.
const BLUETOOTH: [[&str; 2]; 2] = [
    ["btphone:capture_1", "JUCEJack:in_5"],
    ["btphone:capture_2", "JUCEJack:in_6"],
];
const HEADPHONE_ONE: PhysicalPath = PhysicalPath {
    id: "headphones-1",
    kind: PhysicalKind::Output,
    channels: &[
        ["JUCEJack:out_3", "system:playback_3"],
        ["JUCEJack:out_4", "system:playback_4"],
    ],
};

// Duo 1.7.6: recovered from board-selected Mixer::Process and JACK masks.
// 1.7.6 Mixer::Process: 0x659008 (headset input), 0x659014..0x65918c
// (headphone/headset outputs). No live-device qualification.
const DUO_PATHS: [PhysicalPath; 23] = [
    PhysicalPath {
        id: "duo-analog-1",
        ..PATHS[12]
    },
    PhysicalPath {
        id: "duo-analog-2",
        ..PATHS[13]
    },
    PhysicalPath {
        id: "duo-usb-main",
        ..PATHS[1]
    },
    PhysicalPath {
        id: "duo-usb-secondary",
        ..PATHS[3]
    },
    PhysicalPath {
        id: "duo-usb-chat",
        ..PATHS[4]
    },
    PhysicalPath {
        id: "duo-usb-game",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_3", "JUCEJack:in_13"],
            ["usb1in:capture_4", "JUCEJack:in_14"],
        ],
    },
    PhysicalPath {
        id: "duo-usb-music",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_5", "JUCEJack:in_15"],
            ["usb1in:capture_6", "JUCEJack:in_16"],
        ],
    },
    PhysicalPath {
        id: "duo-usb-a",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_7", "JUCEJack:in_17"],
            ["usb1in:capture_8", "JUCEJack:in_18"],
        ],
    },
    PhysicalPath {
        id: "duo-usb-b",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_9", "JUCEJack:in_19"],
            ["usb1in:capture_10", "JUCEJack:in_20"],
        ],
    },
    PhysicalPath {
        id: "duo-callme-1",
        ..PATHS[6]
    },
    PhysicalPath {
        id: "duo-callme-2",
        ..PATHS[7]
    },
    PhysicalPath {
        id: "duo-callme-3",
        ..PATHS[8]
    },
    PhysicalPath {
        id: "duo-wireless-1",
        ..PATHS[9]
    },
    PhysicalPath {
        id: "duo-wireless-2",
        ..PATHS[10]
    },
    PhysicalPath {
        id: "duo-bluetooth",
        ..PATHS[11]
    },
    PhysicalPath {
        id: "duo-headphones-1",
        ..PATHS[5]
    },
    PhysicalPath {
        id: "duo-headset-mic",
        kind: PhysicalKind::Mic,
        channels: &[["system:capture_3", "JUCEJack:in_3"]],
    },
    PhysicalPath {
        id: "duo-headphones-2",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_1", "system:playback_1"],
            ["JUCEJack:out_2", "system:playback_2"],
        ],
    },
    PhysicalPath {
        id: "duo-monitors",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_5", "system:playback_5"],
            ["JUCEJack:out_6", "system:playback_6"],
        ],
    },
    PhysicalPath {
        id: "duo-send-main",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_15", "usb1out:playback_1"],
            ["JUCEJack:out_16", "usb1out:playback_2"],
        ],
    },
    PhysicalPath {
        id: "duo-send-chat",
        ..PATHS[2]
    },
    PhysicalPath {
        id: "duo-send-secondary",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_19", "usb2out:playback_1"],
            ["JUCEJack:out_20", "usb2out:playback_2"],
        ],
    },
    PhysicalPath {
        id: "duo-headset",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_8", "system:playback_8"],
            ["JUCEJack:out_7", "system:playback_7"],
        ],
    },
];

const PATHS: [PhysicalPath; 25] = [
    PhysicalPath {
        id: "mic-1",
        kind: PhysicalKind::Mic,
        channels: &[["system:capture_4", "JUCEJack:in_4"]],
    },
    PhysicalPath {
        id: "usb-main",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_1", "JUCEJack:in_7"],
            ["usb1in:capture_2", "JUCEJack:in_8"],
        ],
    },
    PhysicalPath {
        id: "send-chat",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_17", "usb1-chat-out:playback_1"],
            ["JUCEJack:out_18", "usb1-chat-out:playback_2"],
        ],
    },
    PhysicalPath {
        id: "usb-secondary",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb2in:capture_1", "JUCEJack:in_9"],
            ["usb2in:capture_2", "JUCEJack:in_10"],
        ],
    },
    PhysicalPath {
        id: "usb-chat",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1-chat-in:capture_1", "JUCEJack:in_11"],
            ["usb1-chat-in:capture_2", "JUCEJack:in_12"],
        ],
    },
    HEADPHONE_ONE,
    PhysicalPath {
        id: "callme-1",
        kind: PhysicalKind::Playback,
        channels: &[
            ["sip-audio-in:capture_1", "JUCEJack:in_29"],
            ["sip-audio-in:capture_2", "JUCEJack:in_30"],
        ],
    },
    PhysicalPath {
        id: "callme-2",
        kind: PhysicalKind::Playback,
        channels: &[
            ["sip-audio-in:capture_3", "JUCEJack:in_31"],
            ["sip-audio-in:capture_4", "JUCEJack:in_32"],
        ],
    },
    PhysicalPath {
        id: "callme-3",
        kind: PhysicalKind::Playback,
        channels: &[
            ["sip-audio-in:capture_5", "JUCEJack:in_33"],
            ["sip-audio-in:capture_6", "JUCEJack:in_34"],
        ],
    },
    PhysicalPath {
        id: "wireless-1",
        kind: PhysicalKind::Mic,
        channels: &[["RodeRfSPI:TX1", "JUCEJack:in_35"]],
    },
    PhysicalPath {
        id: "wireless-2",
        kind: PhysicalKind::Mic,
        channels: &[["RodeRfSPI:TX2", "JUCEJack:in_36"]],
    },
    PhysicalPath {
        id: "bluetooth",
        kind: PhysicalKind::Playback,
        channels: &BLUETOOTH,
    },
    PhysicalPath {
        id: "analog-1",
        kind: PhysicalKind::Mic,
        channels: &[["system:capture_1", "JUCEJack:in_1"]],
    },
    PhysicalPath {
        id: "analog-2",
        kind: PhysicalKind::Mic,
        channels: &[["system:capture_2", "JUCEJack:in_2"]],
    },
    PhysicalPath {
        id: "analog-3",
        kind: PhysicalKind::Mic,
        channels: &[["system:capture_3", "JUCEJack:in_3"]],
    },
    PhysicalPath {
        id: "headphones-3",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_1", "system:playback_1"],
            ["JUCEJack:out_2", "system:playback_2"],
        ],
    },
    PhysicalPath {
        id: "headphones-2",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_11", "system:playback_11"],
            ["JUCEJack:out_12", "system:playback_12"],
        ],
    },
    PhysicalPath {
        id: "headphones-4",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_7", "system:playback_7"],
            ["JUCEJack:out_8", "system:playback_8"],
        ],
    },
    PhysicalPath {
        id: "monitors",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_5", "system:playback_5"],
            ["JUCEJack:out_9", "system:playback_9"],
        ],
    },
    PhysicalPath {
        id: "send-main",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_15", "usb1out:playback_1"],
            ["JUCEJack:out_16", "usb1out:playback_2"],
        ],
    },
    PhysicalPath {
        id: "send-secondary",
        kind: PhysicalKind::Output,
        channels: &[
            ["JUCEJack:out_19", "usb2out:playback_1"],
            ["JUCEJack:out_20", "usb2out:playback_2"],
        ],
    },
    PhysicalPath {
        id: "usb-game",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_3", "JUCEJack:in_13"],
            ["usb1in:capture_4", "JUCEJack:in_14"],
        ],
    },
    PhysicalPath {
        id: "usb-music",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_5", "JUCEJack:in_15"],
            ["usb1in:capture_6", "JUCEJack:in_16"],
        ],
    },
    PhysicalPath {
        id: "usb-a",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_7", "JUCEJack:in_17"],
            ["usb1in:capture_8", "JUCEJack:in_18"],
        ],
    },
    PhysicalPath {
        id: "usb-b",
        kind: PhysicalKind::Playback,
        channels: &[
            ["usb1in:capture_9", "JUCEJack:in_19"],
            ["usb1in:capture_10", "JUCEJack:in_20"],
        ],
    },
];
