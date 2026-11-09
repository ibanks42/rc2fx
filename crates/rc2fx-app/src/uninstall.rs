//! What uninstalling RC2FX on Windows offers to remove: the data RC2FX keeps
//! for this user (downloaded firmware, backups of the mixer's RC2FX settings,
//! the installer's journal and known_hosts, preferences) and the SSH key pair
//! it made for the mixer's developer option.
//!
//! Velopack runs this just before it removes the app, and stops it after 60
//! seconds (Update.exe's limit for this hook). The question defaults to
//! keeping everything, so a silent or unattended uninstall keeps it too. A
//! folder set with `RC2FX_INSTALLER_WORK` is the user's own and never
//! removed.
use std::path::PathBuf;

/// Everything to remove, if present.
fn targets() -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if let Some(data) = rc2fx_installer::default_work().and_then(|w| w.parent().map(PathBuf::from))
    {
        targets.push(data);
    }
    let key = rc2fx_installer::default_key();
    if key.is_absolute() {
        let mut public = key.clone().into_os_string();
        public.push(".pub");
        targets.push(public.into());
        targets.push(key);
    }
    targets
}

fn remove(path: &std::path::Path) -> std::io::Result<()> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Ask whether to delete RC2FX's data too, and do so if the user agrees.
#[cfg(target_os = "windows")]
pub fn offer_cleanup() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
    };
    let targets: Vec<PathBuf> = targets().into_iter().filter(|p| p.exists()).collect();
    if targets.is_empty() {
        return;
    }
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let ask = |text: &str, flags| {
        let (text, title) = (wide(text), wide("Uninstall RC2FX"));
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags) }
    };
    let question = "Also delete RC2FX's data on this PC?\n\n\
        This deletes downloaded firmware, the backups of your mixer's RC2FX settings, \
        your RC2FX preferences and the SSH key RC2FX made for the mixer's developer \
        option.\n\nChoose No to keep them in case you install RC2FX again.";
    let flags = MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND;
    if ask(question, flags) != IDYES {
        return;
    }
    let failed: Vec<String> = targets
        .iter()
        .filter(|path| remove(path).is_err())
        .map(|path| path.display().to_string())
        .collect();
    if !failed.is_empty() {
        let text = format!(
            "Some RC2FX data could not be deleted. You can delete it yourself:\n\n{}",
            failed.join("\n")
        );
        ask(&text, MB_TOPMOST | MB_SETFOREGROUND);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rc2fx_data_and_its_own_key_pair_are_targeted() {
        let targets = targets();
        for target in &targets {
            let text = target.to_string_lossy();
            assert!(
                text.ends_with("RC2FX")
                    || text.ends_with("rc2fx_rodecaster_pro_ii")
                    || text.ends_with("rc2fx_rodecaster_pro_ii.pub"),
                "unexpected target {text}"
            );
        }
    }

    #[test]
    fn removing_what_is_already_gone_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remove(&dir.path().join("missing")).is_ok());
        let folder = dir.path().join("RC2FX");
        std::fs::create_dir_all(folder.join("installer")).unwrap();
        std::fs::write(folder.join("view.json"), "{}").unwrap();
        remove(&folder).unwrap();
        assert!(!folder.exists());
    }
}
