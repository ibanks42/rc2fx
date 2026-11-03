use rc2fx_core::{Config, Target, TargetKind};

fn enabled(id: &str, kind: TargetKind, channels: u8) -> Target {
    let mut target = Target::new(id, id, kind, channels);
    target.suppression.enabled = true;
    target
}

#[test]
fn one_mono_mic_or_chat_voice_mix_is_eligible() {
    for target in [
        enabled("analog-1", TargetKind::Mic, 1),
        enabled("mic-1", TargetKind::Mic, 1),
        enabled("send-chat", TargetKind::Output, 2),
        enabled("duo-send-chat", TargetKind::Output, 2),
    ] {
        assert!(target.supports_suppression());
        target.validate().unwrap();
    }
    for target in [
        enabled("headphones-1", TargetKind::Output, 2),
        enabled("usb-chat", TargetKind::Playback, 2),
        enabled("send-chat", TargetKind::Output, 1),
        enabled("stereo-mic", TargetKind::Mic, 2),
    ] {
        assert!(!target.supports_suppression());
        assert!(target.validate().is_err());
    }
}

#[test]
fn saved_enable_budget_is_global_even_when_bypassed_or_deselected() {
    let first = enabled("analog-1", TargetKind::Mic, 1);
    for second in [
        enabled("mic-1", TargetKind::Mic, 1),
        enabled("send-chat", TargetKind::Output, 2),
    ] {
        let mut config = Config {
            targets: vec![first.clone(), second],
            selected_targets: vec![],
            ..Config::default()
        };
        config.targets[1].bypass = true;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("only one")
        );
        config.targets[0].suppression.enabled = false;
        config.validate().unwrap();
    }
}
