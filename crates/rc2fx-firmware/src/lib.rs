//! Firmware-specific addresses and ABI identities, checked before audio changes.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};

mod model;
pub use model::Model;

#[cfg(any(feature = "embedded-catalog", test))]
pub const DEFAULT_CATALOG: &str = include_str!("../../../config/firmware-profiles.json");
pub type Result<T> = std::result::Result<T, String>;

/// Set by the RC2FX launcher only for an explicitly opted-in installation.
/// This never supplies or overrides board identity.
pub fn experimental_duo_from_env() -> bool {
    std::env::var("RC2FX_EXPERIMENTAL_DUO").as_deref() == Ok("1")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Catalog {
    pub schema_version: u32,
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub minimum_app_version: String,
    pub id: String,
    pub model: String,
    pub version: String,
    pub mixer_sha256: String,
    pub launcher_sha256: String,
    pub archive_sha256: String,
    pub archive_md5: String,
    pub runtime: Option<Runtime>,
    pub notes: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Runtime {
    pub callback_abi: String,
    pub route_layout: String,
    pub source_enum: String,
    pub sample_rate: u32,
    pub block_frames: u32,
    pub show_path: String,
    pub callback: Site,
    pub job_lifetime: JobLifetime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobLifetime {
    pub abi: String,
    pub enqueue: Site,
    pub manager: Site,
    pub invoker: Site,
    pub body: Site,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub offset: String,
    pub expected: String,
}

fn hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Site {
    pub fn offset(&self) -> Result<usize> {
        let value = self
            .offset
            .strip_prefix("0x")
            .ok_or("site offset needs 0x prefix")?;
        let offset = usize::from_str_radix(value, 16).map_err(|e| e.to_string())?;
        if offset == 0 || offset >= 64 * 1024 * 1024 || offset % 4 != 0 {
            return Err(
                "site offset must be an aligned executable-relative address below 64 MiB".into(),
            );
        }
        Ok(offset)
    }

    pub fn bytes(&self) -> Result<[u8; 16]> {
        if !hex(&self.expected, 32) {
            return Err("site requires exactly 16 original instruction bytes".into());
        }
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&self.expected[index * 2..index * 2 + 2], 16)
                .map_err(|e| e.to_string())?;
        }
        Ok(bytes)
    }
}

impl Runtime {
    pub fn sites(&self) -> [&Site; 5] {
        [
            &self.callback,
            &self.job_lifetime.enqueue,
            &self.job_lifetime.manager,
            &self.job_lifetime.invoker,
            &self.job_lifetime.body,
        ]
    }

    fn validate(&self) -> Result<()> {
        if self.callback_abi != "jack-process-v1"
            || !matches!(
                (self.route_layout.as_str(), self.source_enum.as_str()),
                ("pro-ii-25-v1", "pro-ii-sources-v1") | ("duo-23-v1", "duo-sources-v1")
            )
            || self.job_lifetime.abi != "capture-values-v1"
            || self.sample_rate != 48_000
            || self.block_frames != 112
        {
            return Err("profile requests a callback, route, source, frame or job ABI this build does not implement".into());
        }
        if !self.show_path.starts_with("/Application/update-staging/")
            || self.show_path.contains("..")
            || !self
                .show_path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        {
            return Err("invalid saved-show path".into());
        }
        let mut offsets = HashSet::new();
        for site in self.sites() {
            if !offsets.insert(site.offset()?) {
                return Err("duplicate hook site".into());
            }
            site.bytes()?;
        }
        // The 16-byte trampoline replays these instructions at a new address.
        // PC-relative branches/addressing/literal loads need a relocator that
        // capture-values-v1 deliberately does not implement.
        for bytes in self.job_lifetime.enqueue.bytes()?.as_chunks::<4>().0 {
            let instruction = u32::from_le_bytes(*bytes);
            if instruction & 0x7c00_0000 == 0x1400_0000
                || instruction & 0x1f00_0000 == 0x1000_0000
                || instruction & 0xff00_0010 == 0x5400_0000
                || instruction & 0x7e00_0000 == 0x3400_0000
                || instruction & 0x7e00_0000 == 0x3600_0000
                || instruction & 0x3b00_0000 == 0x1800_0000
            {
                return Err(
                    "job enqueuer prologue needs unsupported PC-relative relocation".into(),
                );
            }
        }
        Ok(())
    }
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 128 * 1024 {
            return Err("firmware catalog exceeds 128 KiB".into());
        }
        let catalog: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if catalog.schema_version != 1 || catalog.profiles.is_empty() || catalog.profiles.len() > 64
        {
            return Err("unsupported or empty firmware catalog".into());
        }
        let mut ids = HashSet::new();
        let mut hashes = HashSet::new();
        let mut releases = HashSet::new();
        for profile in &catalog.profiles {
            if profile.id.is_empty()
                || profile.id.len() > 96
                || !profile
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b))
                || Model::parse(&profile.model).is_err()
                || !hex(&profile.mixer_sha256, 64)
                || !hex(&profile.launcher_sha256, 64)
                || !hex(&profile.archive_sha256, 64)
                || !hex(&profile.archive_md5, 32)
                || !ids.insert(&profile.id)
                || !hashes.insert((&profile.model, &profile.mixer_sha256))
                || !releases.insert((&profile.model, &profile.version))
            {
                return Err("invalid or duplicate firmware profile identity".into());
            }
            if let Some(runtime) = &profile.runtime {
                let model = Model::parse(&profile.model)?;
                if runtime.route_layout != model.route_layout()
                    || runtime.source_enum != model.source_enum()
                {
                    return Err("runtime routing does not match the device model".into());
                }
                runtime
                    .validate()
                    .map_err(|e| format!("{}: {e}", profile.id))?;
            }
        }
        Ok(catalog)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn select<'a>(&'a self, image: &[u8]) -> Result<&'a Profile> {
        let hash = format!("{:x}", Sha256::digest(image));
        let mut matches = self.profiles.iter().filter(|p| p.mixer_sha256 == hash);
        let profile = matches
            .next()
            .ok_or_else(|| format!("unrecognized mixer executable SHA-256 {hash}"))?;
        if matches.next().is_some() {
            return Err("ambiguous mixer executable: select using verified device model".into());
        }
        Model::parse(&profile.model)?.require_runtime()?;
        profile.verify_image(image)?;
        Ok(profile)
    }

    fn identify(&self, model: Model, image: &[u8]) -> Result<&Profile> {
        let hash = format!("{:x}", Sha256::digest(image));
        self.profiles
            .iter()
            .find(|p| p.model == model.id() && p.mixer_sha256 == hash)
            .ok_or_else(|| {
                format!(
                    "unrecognized {} mixer executable SHA-256 {hash}",
                    model.id()
                )
            })
    }

    /// Runtime admission needs a board identity, not just a shared executable.
    pub fn select_for_model(&self, model: Model, image: &[u8]) -> Result<&Profile> {
        self.select_for_model_with(model, image, false)
    }

    /// The opt-in admits only the named model's own profile and route layout.
    pub fn select_for_model_with(
        &self,
        model: Model,
        image: &[u8],
        experimental_duo: bool,
    ) -> Result<&Profile> {
        model.require_runtime_with(experimental_duo)?;
        let profile = self.identify(model, image)?;
        profile.verify_image(image)?;
        Ok(profile)
    }

    /// Offline-only executable inspection. Reuse a qualified executable's
    /// instruction checks, but do NOT grant its physical routes to this model.
    /// The returned release can still have `runtime: None`.
    pub fn inspect_for_model(
        &self,
        model: Model,
        image: &[u8],
        launcher: &[u8],
    ) -> Result<&Profile> {
        let profile = self.identify(model, image)?;
        profile.verify_launcher(launcher)?;
        let abi = self
            .profiles
            .iter()
            .find(|p| {
                p.mixer_sha256 == profile.mixer_sha256
                    && p.launcher_sha256 == profile.launcher_sha256
                    && p.runtime.is_some()
            })
            .ok_or("no qualified executable ABI matches this release")?;
        abi.verify_image(image)?;
        Ok(profile)
    }
}

impl Profile {
    pub fn verify_image(&self, image: &[u8]) -> Result<()> {
        if format!("{:x}", Sha256::digest(image)) != self.mixer_sha256 {
            return Err("mixer executable differs from its profile hash".into());
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| format!("{} is not runtime-qualified: {}", self.id, self.notes))?;
        runtime.validate()?;
        let model = Model::parse(&self.model)?;
        if runtime.route_layout != model.route_layout()
            || runtime.source_enum != model.source_enum()
        {
            return Err("runtime routing does not match the device model".into());
        }
        for site in runtime.sites() {
            if image_bytes(image, site.offset()?)? != site.bytes()? {
                return Err(format!(
                    "{}: original instructions differ at {}",
                    self.id, site.offset
                ));
            }
        }
        Ok(())
    }

    pub fn verify_launcher(&self, bytes: &[u8]) -> Result<()> {
        if format!("{:x}", Sha256::digest(bytes)) != self.launcher_sha256 {
            return Err(format!(
                "{}: audio launcher differs from its profile hash",
                self.id
            ));
        }
        Ok(())
    }
}

/// Resolve a virtual address through executable ELF PT_LOAD segments, never
/// assume that the file offset happens to equal the executable-relative offset.
fn image_bytes(image: &[u8], address: usize) -> Result<&[u8]> {
    let read = |offset: usize, size: usize| {
        image
            .get(offset..offset.saturating_add(size))
            .ok_or_else(|| "truncated ELF image".to_string())
    };
    let u16_at =
        |offset| -> Result<u16> { Ok(u16::from_le_bytes(read(offset, 2)?.try_into().unwrap())) };
    let u32_at =
        |offset| -> Result<u32> { Ok(u32::from_le_bytes(read(offset, 4)?.try_into().unwrap())) };
    let u64_at = |offset| -> Result<usize> {
        usize::try_from(u64::from_le_bytes(read(offset, 8)?.try_into().unwrap()))
            .map_err(|e| e.to_string())
    };
    if read(0, 6)? != b"\x7fELF\x02\x01" || u16_at(16)? != 3 || u16_at(18)? != 183 {
        return Err("profile needs an AArch64 little-endian ELF64 PIE".into());
    }
    let table = u64_at(32)?;
    let stride = usize::from(u16_at(54)?);
    let count = usize::from(u16_at(56)?);
    if stride < 56 || count > 128 {
        return Err("invalid ELF program header table".into());
    }
    for index in 0..count {
        let start = table
            .checked_add(index * stride)
            .ok_or("ELF header overflow")?;
        read(start, 56)?;
        if u32_at(start)? != 1 || u32_at(start + 4)? & 1 == 0 {
            continue;
        }
        let file = u64_at(start + 8)?;
        let base = u64_at(start + 16)?;
        let size = u64_at(start + 32)?;
        if let Some(relative) = address.checked_sub(base)
            && relative.checked_add(16).is_some_and(|end| end <= size)
        {
            return read(
                file.checked_add(relative).ok_or("ELF segment overflow")?,
                16,
            );
        }
    }
    Err("hook site is outside executable file-backed ELF code".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rejects_unknown_abis_duplicate_identities_and_unsafe_trampolines() {
        let defaults = Catalog::parse(DEFAULT_CATALOG).unwrap();
        assert!(defaults.profiles[0].runtime.is_some());
        assert!(defaults.profiles[1].runtime.is_some());
        let mut bad = defaults.clone();
        bad.profiles[0].runtime.as_mut().unwrap().callback_abi = "unknown".into();
        assert!(Catalog::parse(&serde_json::to_string(&bad).unwrap()).is_err());
        let mut bad = defaults.clone();
        bad.profiles.push(defaults.profiles[0].clone());
        assert!(Catalog::parse(&serde_json::to_string(&bad).unwrap()).is_err());
        let mut bad = defaults;
        bad.profiles[0]
            .runtime
            .as_mut()
            .unwrap()
            .job_lifetime
            .enqueue
            .expected = "00000014000000000000000000000000".into(); // b +0 cannot be replayed elsewhere
        assert!(Catalog::parse(&serde_json::to_string(&bad).unwrap()).is_err());
    }

    fn fixture() -> (Catalog, Vec<u8>) {
        let mut catalog = Catalog::parse(DEFAULT_CATALOG).unwrap();
        catalog.profiles.truncate(1);
        let runtime = catalog.profiles[0].runtime.as_mut().unwrap();
        let sites = [
            &mut runtime.callback,
            &mut runtime.job_lifetime.enqueue,
            &mut runtime.job_lifetime.manager,
            &mut runtime.job_lifetime.invoker,
            &mut runtime.job_lifetime.body,
        ];
        let mut image = vec![0; 0x400];
        image[..6].copy_from_slice(b"\x7fELF\x02\x01");
        image[16..18].copy_from_slice(&3u16.to_le_bytes());
        image[18..20].copy_from_slice(&183u16.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes());
        image[54..56].copy_from_slice(&56u16.to_le_bytes());
        image[56..58].copy_from_slice(&1u16.to_le_bytes());
        image[64..68].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        image[68..72].copy_from_slice(&5u32.to_le_bytes()); // PF_R|PF_X
        image[72..80].copy_from_slice(&0x200u64.to_le_bytes());
        image[80..88].copy_from_slice(&0x1000u64.to_le_bytes());
        image[96..104].copy_from_slice(&0x100u64.to_le_bytes());
        for (index, site) in sites.into_iter().enumerate() {
            site.offset = format!("0x{:x}", 0x1000 + 16 * index);
            image[0x200 + index * 16..0x210 + index * 16].copy_from_slice(&site.bytes().unwrap());
        }
        catalog.profiles[0].mixer_sha256 = format!("{:x}", Sha256::digest(&image));
        (catalog, image)
    }

    #[test]
    fn shared_executable_is_not_a_device_identity_or_runtime_qualification() {
        let (mut catalog, image) = fixture();
        let launcher = b"test launcher";
        catalog.profiles[0].launcher_sha256 = format!("{:x}", Sha256::digest(launcher));
        let mut duo = catalog.profiles[0].clone();
        duo.id = "rodecaster-duo-1.7.6".into();
        duo.model = Model::Duo.id().into();
        duo.runtime = None;
        catalog.profiles.push(duo);
        let catalog = Catalog::parse(&serde_json::to_string(&catalog).unwrap()).unwrap();
        assert!(catalog.select(&image).unwrap_err().contains("ambiguous"));
        assert_eq!(
            catalog
                .select_for_model(Model::ProIi, &image)
                .unwrap()
                .model,
            Model::ProIi.id()
        );
        assert!(
            catalog
                .select_for_model(Model::Duo, &image)
                .unwrap_err()
                .contains("experimental")
        );
        let inspected = catalog
            .inspect_for_model(Model::Duo, &image, launcher)
            .unwrap();
        assert_eq!(inspected.model, Model::Duo.id());
        assert!(inspected.runtime.is_none());
        assert!(
            catalog
                .inspect_for_model(Model::Duo, &image, b"wrong launcher")
                .is_err()
        );
        let mut bad = catalog.clone();
        bad.profiles[1].runtime = bad.profiles[0].runtime.clone();
        assert!(Catalog::parse(&serde_json::to_string(&bad).unwrap()).is_err());
        let mut duplicate = catalog.clone();
        let mut profile = duplicate.profiles[0].clone();
        profile.id = "another-id".into();
        duplicate.profiles.push(profile);
        assert!(Catalog::parse(&serde_json::to_string(&duplicate).unwrap()).is_err());
    }

    #[test]
    fn experimental_opt_in_never_grants_the_pro_ii_layout_to_a_duo() {
        let (mut catalog, image) = fixture();
        let mut duo = catalog.profiles[0].clone();
        duo.id = "duo-fixture".into();
        duo.model = Model::Duo.id().into();
        duo.runtime.as_mut().unwrap().route_layout = Model::Duo.route_layout().into();
        duo.runtime.as_mut().unwrap().source_enum = Model::Duo.source_enum().into();
        catalog.profiles.push(duo);
        assert!(catalog.select_for_model(Model::Duo, &image).is_err());
        assert_eq!(
            catalog
                .select_for_model_with(Model::Duo, &image, true)
                .unwrap()
                .model,
            Model::Duo.id()
        );
        assert!(catalog.select(&image).is_err());
        catalog.profiles[1].runtime = catalog.profiles[0].runtime.clone();
        assert!(
            catalog
                .select_for_model_with(Model::Duo, &image, true)
                .is_err()
        );
        assert!(Catalog::parse(&serde_json::to_string(&catalog).unwrap()).is_err());
        assert!(
            Model::from_board(
                Model::Duo.hostname(),
                &format!("FDT {}", Model::ProIi.board_dtb())
            )
            .is_err()
        );
    }

    #[test]
    fn resolves_executable_virtual_offsets_and_refuses_hash_or_instruction_mismatches() {
        let (mut catalog, mut image) = fixture();
        catalog.select(&image).unwrap();
        image[0x200] ^= 1;
        assert!(catalog.select(&image).unwrap_err().contains("unrecognized"));
        catalog.profiles[0].mixer_sha256 = format!("{:x}", Sha256::digest(&image));
        assert!(
            catalog
                .select(&image)
                .unwrap_err()
                .contains("instructions differ")
        );
    }

    #[test]
    fn known_but_unqualified_and_non_executable_sites_are_refused() {
        let (mut catalog, mut image) = fixture();
        catalog.profiles[0].runtime = None;
        assert!(
            catalog
                .select(&image)
                .unwrap_err()
                .contains("not runtime-qualified")
        );
        let (mut catalog, _) = fixture();
        image[68..72].copy_from_slice(&4u32.to_le_bytes()); // data, no PF_X
        catalog.profiles[0].mixer_sha256 = format!("{:x}", Sha256::digest(&image));
        assert!(
            catalog
                .select(&image)
                .unwrap_err()
                .contains("outside executable")
        );
    }
}
