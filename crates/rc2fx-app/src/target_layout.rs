use rc2fx_core::{Target, TargetKind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Microphones,
    Sources,
    Headphones,
    Outputs,
}

/// What the app calls the mixer's other outputs, the same on a Pro II and a
/// Duo. Configs saved before these names carry older ones.
pub(super) fn output_label(id: &str) -> Option<&'static str> {
    match id.trim_start_matches("duo-") {
        "monitors" => Some("Monitors"),
        "send-main" => Some("Main Mix Out"),
        "send-chat" => Some("Chat Mix Out"),
        "send-secondary" => Some("Secondary Mix Out"),
        _ => None,
    }
}

/// A channel name as the app shows it: every word capitalised, like "Main
/// Mix Out", and USB in capitals ("USB a" becomes "USB A"). Words already
/// carrying capitals keep them ("CallMe", "RØDE"). Display only: saved
/// settings keep the mixer's own names.
pub(super) fn display_name(name: &str) -> String {
    name.split(' ')
        .map(|word| {
            if word.eq_ignore_ascii_case("usb") {
                return "USB".to_owned();
            }
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Group {
    pub const ALL: [Self; 4] = [
        Self::Microphones,
        Self::Sources,
        Self::Headphones,
        Self::Outputs,
    ];

    pub fn of(target: &Target) -> Self {
        match target.kind {
            TargetKind::Mic => Self::Microphones,
            TargetKind::Playback => Self::Sources,
            TargetKind::Output
                if matches!(
                    target.id.as_str(),
                    "headphones-1" | "headphones-2" | "headphones-3" | "headphones-4"
                ) =>
            {
                Self::Headphones
            }
            TargetKind::Output => Self::Outputs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Microphones => "MICROPHONES",
            Self::Sources => "SOURCES",
            Self::Headphones => "HEADPHONES",
            Self::Outputs => "OUTPUTS",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn names_are_title_case_with_usb_in_capitals() {
        assert_eq!(display_name("USB a"), "USB A");
        assert_eq!(display_name("usb main"), "USB Main");
        assert_eq!(display_name("Main Mix Out"), "Main Mix Out");
        assert_eq!(display_name("CallMe 1"), "CallMe 1");
        assert_eq!(display_name("headphones 2"), "Headphones 2");
        assert_eq!(display_name("SMART pads"), "SMART Pads");
    }
}
