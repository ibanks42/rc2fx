//! Static checks that a cross-built binary will load on the mixer: AArch64,
//! only the firmware's system libraries, no symbol versions newer than the
//! firmware provides, and no build-host search paths or text relocations.
use crate::Result;
use object::{
    Endianness,
    elf::{self, FileHeader64},
    read::elf::{Dyn, FileHeader, ProgramHeader, SectionHeader},
};
use std::path::Path;

/// Libraries firmware 1.7.6 ships in /lib.
const LIBRARIES: &[&str] = &[
    "libc.so.6",
    "libm.so.6",
    "libdl.so.2",
    "libpthread.so.0",
    "librt.so.1",
    "libgcc_s.so.1",
    "ld-linux-aarch64.so.1",
];
/// Newest symbol versions in the firmware's glibc 2.35 and GCC 11 libgcc_s.
const GLIBC_MINOR: u32 = 35;
const GCC_MAJOR: u32 = 11;
const INTERPRETER: &[u8] = b"/lib/ld-linux-aarch64.so.1";

pub fn check(path: &Path, shared: bool) -> Result {
    let name = path.display();
    let data = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    check_bytes(&data, shared).map_err(|e| format!("{name}: {e}"))?;
    println!("{name}: meets the mixer ABI baseline (AArch64, glibc 2.35, GCC 11)");
    Ok(())
}

fn check_bytes(data: &[u8], shared: bool) -> Result {
    let text = |e: object::read::Error| e.to_string();
    let header = FileHeader64::<Endianness>::parse(data).map_err(text)?;
    let endian = header.endian().map_err(text)?;
    if header.e_machine(endian) != elf::EM_AARCH64 {
        return Err("not an AArch64 binary".into());
    }
    if header.e_type(endian) != elf::ET_DYN {
        return Err("not position independent".into());
    }
    let mut interpreter = None;
    for segment in header.program_headers(endian, data).map_err(text)? {
        if let Some(path) = segment.interpreter(endian, data).map_err(text)? {
            interpreter = Some(path.strip_suffix(b"\0").unwrap_or(path));
        }
    }
    match (shared, interpreter) {
        (true, None) => {}
        (true, Some(_)) => return Err("a shared library must not request an interpreter".into()),
        (false, Some(INTERPRETER)) => {}
        (false, _) => return Err("executable must use /lib/ld-linux-aarch64.so.1".into()),
    }
    let sections = header.sections(endian, data).map_err(text)?;
    let mut dynamic_found = false;
    for section in sections.iter() {
        let Some((entries, link)) = section.dynamic(endian, data).map_err(text)? else {
            continue;
        };
        dynamic_found = true;
        let strings = sections.strings(endian, data, link).map_err(text)?;
        for entry in entries {
            let tag = entry.tag32(endian).unwrap_or(0);
            let value = entry.d_val(endian);
            match tag {
                elf::DT_NEEDED => {
                    let library = entry.string(endian, strings).map_err(text)?;
                    let library = String::from_utf8_lossy(library);
                    if !LIBRARIES.contains(&library.as_ref()) {
                        return Err(format!("needs {library}, which the mixer does not ship"));
                    }
                }
                elf::DT_RPATH | elf::DT_RUNPATH => {
                    return Err("carries a library search path".into());
                }
                elf::DT_TEXTREL => return Err("has text relocations".into()),
                elf::DT_FLAGS if value & u64::from(elf::DF_TEXTREL) != 0 => {
                    return Err("has text relocations".into());
                }
                elf::DT_FLAGS_1 if shared && value & u64::from(elf::DF_1_PIE) != 0 => {
                    return Err("is a PIE executable, not a library".into());
                }
                _ => {}
            }
        }
    }
    if !dynamic_found {
        return Err("is not dynamically linked".into());
    }
    for section in sections.iter() {
        let Some((mut needs, link)) = section.gnu_verneed(endian, data).map_err(text)? else {
            continue;
        };
        let strings = sections.strings(endian, data, link).map_err(text)?;
        while let Some((_, mut versions)) = needs.next().map_err(text)? {
            while let Some(version) = versions.next().map_err(text)? {
                let version = version.name(endian, strings).map_err(text)?;
                let version = String::from_utf8_lossy(version);
                if !supported(&version) {
                    return Err(format!(
                        "needs symbol version {version}, newer than the firmware"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn supported(version: &str) -> bool {
    let number = |text: &str, index: usize| {
        text.split('.')
            .nth(index)
            .and_then(|part| part.parse::<u32>().ok())
    };
    if let Some(rest) = version.strip_prefix("GLIBC_") {
        return number(rest, 0) == Some(2)
            && number(rest, 1).is_some_and(|minor| minor <= GLIBC_MINOR);
    }
    if let Some(rest) = version.strip_prefix("GCC_") {
        return number(rest, 0).is_some_and(|major| major <= GCC_MAJOR);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::supported;

    #[test]
    fn accepts_only_firmware_symbol_versions() {
        for version in [
            "GLIBC_2.17",
            "GLIBC_2.35",
            "GCC_3.0",
            "GCC_4.2.0",
            "GCC_11.0",
        ] {
            assert!(supported(version), "{version}");
        }
        for version in [
            "GLIBC_2.36",
            "GLIBC_2.39",
            "GLIBC_PRIVATE",
            "GCC_12.0",
            "CXXABI_1.3",
        ] {
            assert!(!supported(version), "{version}");
        }
    }
}
