//! Official RØDE releases, verified downloads and the local firmware patch.

use crate::{
    Catalog, Engine, KEY_MEMBER, MAX_DOWNLOAD, Prepared, Profile, Progress, Release, Result, bail,
    files::{self, Algorithm, Temp},
    runtime::{Entry, image_entries},
};
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use rc2fx_firmware::Model;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read, Write},
    path::Path,
    time::Duration,
};
pub(crate) fn valid_url(value: &str) -> bool {
    let Ok(u) = url::Url::parse(value) else {
        return false;
    };
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    // A bare origin has no path; url normalizes it to "/".
    let has_path = value
        .strip_prefix("https://")
        .is_some_and(|rest| rest.contains('/'));
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && has_path
        && (host == "rode.com" || host.ends_with(".rode.com"))
        && u.port().is_none_or(|p| p == 443)
}

pub(crate) fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20 * 60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 || !valid_url(attempt.url().as_str()) {
                attempt.error("redirect outside RØDE HTTPS")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

pub(crate) fn supported(p: &Profile) -> bool {
    let Some(r) = &p.runtime else {
        return false;
    };
    let Ok(model) = Model::parse(&p.model) else {
        return false;
    };
    r.callback_abi == "jack-process-v1"
        && r.route_layout == model.route_layout()
        && r.source_enum == model.source_enum()
        && r.job_lifetime.abi == "capture-values-v1"
        && r.sample_rate == 48000
        && r.block_frames == 112
}

pub(crate) fn app_supports(p: &Profile, app_version: &str) -> bool {
    p.minimum_app_version.is_empty() || matches!(crate::compare_versions(app_version, &p.minimum_app_version),
        Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater))
}

fn valid_version(v: &str) -> bool {
    let parts: Vec<_> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
pub(crate) fn location(value: &str, suffix: &str, version: &str) -> Result<String> {
    location_for_model(Model::ProIi, value, suffix, version)
}

pub(crate) fn location_for_model(
    model: Model,
    value: &str,
    suffix: &str,
    version: &str,
) -> Result<String> {
    let v = if value.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let ok = url::Url::parse(&v).is_ok_and(|u| {
        valid_url(&v)
            && u.host_str() == Some("update.rode.com")
            && valid_version(version)
            && u.path() == format!("/{}/{version}/{suffix}", model.firmware_directory())
            && u.query().is_none()
            && u.fragment().is_none()
    });
    if !ok {
        bail!("unexpected {} firmware location", model.id());
    }
    Ok(v)
}

/// Read a gzip-compressed tar archive, enforcing an expansion limit and the
/// gzip trailer checks. `raw` presents GNU long-name and PAX records as their
/// own entries so they can be copied verbatim.
pub(crate) fn walk_archive(
    path: &Path,
    limit: u64,
    raw: bool,
    mut visit: impl FnMut(&mut tar::Entry<'_, &mut dyn Read>) -> Result<()>,
) -> Result<()> {
    let gz = MultiGzDecoder::new(io::BufReader::new(File::open(path)?));
    let mut limited = gz.take(limit + 1);
    {
        let reader: &mut dyn Read = &mut limited;
        let mut archive = tar::Archive::new(reader);
        let mut entries = archive.entries()?;
        if raw {
            entries = entries.raw(true);
        }
        for entry in entries {
            visit(&mut entry?)?;
        }
    }
    // Read through the trailer, enforcing gzip CRC and the expansion limit.
    io::copy(&mut limited, &mut io::sink())?;
    if limited.limit() == 0 {
        bail!("archive expansion limit exceeded");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn entry_name(entry: &tar::Entry<'_, &mut dyn Read>) -> String {
    String::from_utf8_lossy(&entry.path_bytes()).into_owned()
}

pub(crate) fn normalized(name: &str) -> &str {
    let name = name.strip_prefix("./").unwrap_or(name);
    name.strip_suffix('/').unwrap_or(name)
}

fn regular(header: &tar::Header) -> bool {
    header.entry_type() == tar::EntryType::Regular
}

/// Parse PAX extended-header records ("<len> <key>=<value>\n").
fn pax_records(data: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let mut records = HashMap::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or("invalid PAX record")?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("invalid PAX record")?;
        if len <= space + 1 || len > rest.len() || rest[len - 1] != b'\n' {
            bail!("invalid PAX record");
        }
        let record = &rest[space + 1..len - 1];
        let eq = record
            .iter()
            .position(|&b| b == b'=')
            .ok_or("invalid PAX record")?;
        records.insert(
            String::from_utf8_lossy(&record[..eq]).into_owned(),
            record[eq + 1..].to_vec(),
        );
        rest = &rest[len..];
    }
    Ok(records)
}

/// Write a copy of `src` to `dst` with `extra` added and the empty root
/// authorized_keys file set to `key` with mode 0600 (left as it is when `key`
/// is empty). Every other record,
/// including long-name and PAX records, is copied verbatim. Returns the
/// SHA-256 of the mixer executable and its launcher.
#[cfg(test)]
pub(crate) fn patch_archive(
    src: &Path,
    dst: &Path,
    key: &[u8],
    extra: &[Entry],
) -> Result<(String, String)> {
    patch_archive_for_model(Model::ProIi, src, dst, key, extra)
}

pub(crate) fn patch_archive_for_model(
    model: Model,
    src: &Path,
    dst: &Path,
    key: &[u8],
    extra: &[Entry],
) -> Result<(String, String)> {
    const IMPORTANT: [&str; 5] = [
        KEY_MEMBER,
        "etc/hostname",
        "etc/ssh/sshd_config",
        "usr/bin/rc_audio_mixer",
        "usr/bin/audio-start.sh",
    ];
    let dir = dst.parent().unwrap_or(Path::new("."));
    let temp = Temp::new(dir, ".firmware-")?;
    let mut builder = tar::Builder::new(GzEncoder::new(temp, Compression::default()));
    let mut seen = HashSet::new();
    let mut hashes = HashMap::new();
    let (mut host_ok, mut ssh_ok) = (false, false);
    // Names supplied by preceding GNU long-name or PAX records.
    let mut long_name: Option<String> = None;
    let mut pax_name: Option<String> = None;
    let mut pax_size = false;
    let mut has_opt = false;
    walk_archive(src, 4 << 30, true, |entry| {
        let header = entry.header().clone();
        let kind = header.entry_type();
        if kind.is_gnu_longname() || kind.is_pax_local_extensions() {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            if kind.is_gnu_longname() {
                let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
                long_name = Some(String::from_utf8_lossy(&data[..end]).into_owned());
            } else {
                let records = pax_records(&data)?;
                pax_size = records.contains_key("size");
                pax_name = records
                    .get("path")
                    .map(|p| String::from_utf8_lossy(p).into_owned());
            }
            builder.append(&header, data.as_slice())?;
            return Ok(());
        }
        let name = pax_name
            .take()
            .or_else(|| long_name.take())
            .unwrap_or_else(|| String::from_utf8_lossy(&header.path_bytes()).into_owned());
        let sized_by_pax = std::mem::take(&mut pax_size);
        let n = normalized(&name);
        if extra.iter().any(|e| e.path() == n) {
            bail!("the firmware already contains {n}");
        }
        if n == "opt" {
            has_opt = true;
        }
        if IMPORTANT.contains(&n)
            && (!seen.insert(n.to_owned()) || !regular(&header) || sized_by_pax)
        {
            bail!("duplicate or nonregular required member {n}");
        }
        if n == KEY_MEMBER || n == "etc/hostname" || n == "etc/ssh/sshd_config" {
            if header.size()? > 1 << 20 {
                bail!("oversized firmware metadata");
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            let mut header = header;
            match n {
                KEY_MEMBER => {
                    if !data.trim_ascii().is_empty() {
                        bail!("authorized_keys is not empty");
                    }
                    // Without a key the file stays as RØDE ships it.
                    if !key.is_empty() {
                        data = key.to_vec();
                        header.set_size(data.len() as u64);
                        header.set_mode(0o600);
                        header.set_cksum();
                    }
                }
                "etc/hostname" => host_ok = data.trim_ascii() == model.hostname().as_bytes(),
                _ => {
                    let text = String::from_utf8_lossy(&data);
                    ssh_ok =
                        text.contains("PermitRootLogin yes") && text.contains("AuthorizedKeysFile");
                }
            }
            builder.append(&header, data.as_slice())?;
            return Ok(());
        }
        if n == "usr/bin/rc_audio_mixer" || n == "usr/bin/audio-start.sh" {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            hashes.insert(n.to_owned(), files::sum(&data));
            builder.append(&header, data.as_slice())?;
            return Ok(());
        }
        builder.append(&header, entry)?;
        Ok(())
    })?;
    if !extra.is_empty() && !has_opt {
        append_entry(
            &mut builder,
            &Entry::Dir {
                path: "opt".into(),
                mode: 0o755,
            },
        )?;
    }
    for entry in extra {
        append_entry(&mut builder, entry)?;
    }
    let mut temp = builder.into_inner()?.finish()?;
    temp.finish()?;
    if !seen.contains(KEY_MEMBER) || !host_ok || !ssh_ok || hashes.len() != 2 {
        bail!("firmware is not the expected {} rootfs", model.label());
    }
    if std::fs::metadata(&temp.path)?.len() > 275 << 20 {
        bail!("patched image exceeds update volume limit");
    }
    temp.persist(dst)?;
    Ok((
        hashes.remove("usr/bin/rc_audio_mixer").unwrap_or_default(),
        hashes.remove("usr/bin/audio-start.sh").unwrap_or_default(),
    ))
}

/// Append one root-owned entry in the image's `./path` style.
fn append_entry<W: Write>(builder: &mut tar::Builder<W>, entry: &Entry) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(crate::now_unix());
    let path = match entry {
        Entry::Dir { path, .. } => format!("./{path}/"),
        Entry::File { path, .. } => format!("./{path}"),
    };
    match entry {
        Entry::Dir { mode, .. } => {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(*mode);
            header.set_size(0);
            builder.append_data(&mut header, path, io::empty())?;
        }
        Entry::File { mode, data, .. } => {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(*mode);
            header.set_size(data.len() as u64);
            builder.append_data(&mut header, path, data.as_slice())?;
        }
    }
    Ok(())
}

/// Counts bytes written to an inner writer, rejecting anything past a limit.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct Bounded<W> {
    pub inner: W,
    pub remaining: u64,
}

impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            return Err(io::Error::other("response exceeds size limit"));
        }
        let n = self.inner.write(buf)?;
        self.remaining -= n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Engine {
    fn client(&mut self) -> Result<reqwest::blocking::Client> {
        if self.http.is_none() {
            self.http = Some(http_client()?);
        }
        Ok(self.http.clone().expect("client was just created"))
    }

    pub(crate) fn get(&mut self, value: &str, limit: u64) -> Result<Vec<u8>> {
        if !valid_url(value) {
            bail!("expected official RØDE HTTPS URL");
        }
        let response = self
            .client()?
            .get(value)
            .timeout(Duration::from_secs(30))
            .send()?;
        if response.status() != reqwest::StatusCode::OK {
            bail!("RØDE returned HTTP {}", response.status().as_u16());
        }
        let mut data = Vec::new();
        response.take(limit + 1).read_to_end(&mut data)?;
        if data.len() as u64 > limit {
            bail!("metadata exceeds size limit");
        }
        Ok(data)
    }

    pub(crate) fn catalog(&self) -> Result<Catalog> {
        self.source.catalog()
    }

    pub(crate) fn releases(&mut self) -> Result<Vec<Release>> {
        self.model
            .require_runtime_with(self.allow_experimental_duo)?;
        self.inspect_releases(self.model)
    }

    /// Read-only discovery, including experimental models. A supported
    /// experimental profile still requires the separate session opt-in.
    pub fn inspect_releases(&mut self, model: Model) -> Result<Vec<Release>> {
        releases_from_catalog(model, &self.catalog()?)
    }
}

/// The bundled, qualified catalog is the only release authority. RØDE's
/// moving manifest must never add choices or retire a version we support.
pub(crate) fn releases_from_catalog(model: Model, c: &Catalog) -> Result<Vec<Release>> {
    let mut all = Vec::new();
    for p in &c.profiles {
        if p.model != model.id() || !supported(p) {
            continue;
        }
        let base = format!(
            "https://update.rode.com/{}/{}",
            model.firmware_directory(),
            p.version
        );
        all.push(Release {
            minimum_app_version: p.minimum_app_version.clone(),
            profile: p.id.clone(),
            model: model.id().into(),
            url: location_for_model(model, &format!("{base}/os.tar.gz"), "os.tar.gz", &p.version)?,
            digest_url: location_for_model(model, &format!("{base}/os.md5"), "os.md5", &p.version)?,
            version: p.version.clone(),
            // Every currently qualified catalog release is stable.
            channel: "stable".into(),
            supported: app_supports(p, env!("CARGO_PKG_VERSION")),
        });
    }
    // Newest qualified stable release is the default, not catalog file order.
    let version_key = |r: &Release| {
        r.version
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    all.sort_by_key(|r| std::cmp::Reverse(version_key(r)));
    Ok(all)
}

impl Engine {
    pub(crate) fn verify_checksum(
        &mut self,
        path: &Path,
        algorithm: Algorithm,
        want: &str,
    ) -> Result<()> {
        let file = File::open(path)?;
        let total = file.metadata()?.len();
        let detail = match algorithm {
            Algorithm::Sha256 => "sha256",
            Algorithm::Md5 => "md5",
        };
        let progress = |completed| Progress {
            phase: "verification".into(),
            completed,
            total,
            detail: detail.into(),
        };
        self.emit(progress(0));
        let mut events = Vec::new();
        let got = files::hash_reader(file, algorithm, |n| events.push(n))?;
        // Report a bounded number of updates after hashing.
        let step = (events.len() / 32).max(1);
        for n in events.into_iter().step_by(step) {
            self.emit(progress(n));
        }
        if want.is_empty() || got != want {
            bail!("checksum mismatch: {}", path.display());
        }
        Ok(())
    }

    /// Download an official release to `dest`, checking RØDE's MD5 and, when
    /// the catalog pins one, its SHA-256. A matching file is kept.
    pub(crate) fn download(&mut self, value: &str, dest: &Path, sha: &str, md: &str) -> Result<()> {
        if !valid_url(value) || md.is_empty() {
            bail!("download requires an official URL and RØDE's checksum");
        }
        if (sha.is_empty() || files::matches(dest, Algorithm::Sha256, sha).is_ok())
            && files::matches(dest, Algorithm::Md5, md).is_ok()
        {
            return Ok(());
        }
        let dir = dest.parent().unwrap_or(Path::new("."));
        let mut temp = Temp::new(dir, ".download-")?;
        let mut response = self.client()?.get(value).send()?;
        if response.status() != reqwest::StatusCode::OK {
            bail!("firmware HTTP {}", response.status().as_u16());
        }
        let total = response.content_length().unwrap_or(0);
        let event = |completed| Progress {
            phase: "download".into(),
            completed,
            total,
            detail: value.into(),
        };
        self.emit(event(0));
        let mut buf = vec![0; 1 << 16];
        let mut n = 0u64;
        let mut last = 0u64;
        loop {
            if self.cancelled() {
                bail!("{}", crate::CANCELLED);
            }
            let read = response.read(&mut buf)?;
            if read == 0 {
                break;
            }
            n += read as u64;
            if n > MAX_DOWNLOAD {
                bail!("firmware exceeds size limit");
            }
            temp.write_all(&buf[..read])?;
            if n - last >= 1 << 20 {
                last = n;
                self.emit(event(n));
            }
        }
        self.emit(event(n));
        temp.finish()?;
        if !sha.is_empty() {
            self.verify_checksum(&temp.path, Algorithm::Sha256, sha)?;
        }
        self.verify_checksum(&temp.path, Algorithm::Md5, md)?;
        std::fs::rename(&temp.path, dest)?;
        temp.path.clear();
        Ok(())
    }

    pub(crate) fn profile(&self, id: &str) -> Result<Profile> {
        self.catalog()?
            .profiles
            .into_iter()
            .find(|p| p.id == id && supported(p) && app_supports(p, env!("CARGO_PKG_VERSION")))
            .ok_or_else(|| crate::Error(format!("firmware profile {id} is not supported")))
    }

    /// What the image for `profile` adds: the runtime, autostart and the
    /// newest settings saved for `serial`. Returns the entries, the runtime
    /// digest and the settings digest (empty without settings).
    fn image_for(&self, profile: &Profile, serial: &str) -> Result<(Vec<Entry>, String, String)> {
        let bundle = self.source.bundle()?;
        let settings = match self.load()?.backup_for(serial) {
            Some(b) => Some(crate::runtime::backup_settings(b)?),
            None => None,
        };
        let digest = settings.as_deref().map(files::sum).unwrap_or_default();
        let (entries, runtime) = image_entries(&bundle, profile, settings.as_deref())?;
        Ok((entries, runtime, digest))
    }

    /// Download and verify the selected release, then build firmware with
    /// RC2FX's runtime, autostart and the newest settings saved for `serial`,
    /// plus RC2FX's SSH key when `ssh` (a developer option). A matching
    /// earlier preparation is reused after verification.
    pub(crate) fn prepare(
        &mut self,
        channel: &str,
        version: &str,
        serial: &str,
        stock: bool,
        ssh: bool,
    ) -> Result<Prepared> {
        self.model
            .require_runtime_with(self.allow_experimental_duo)?;
        // Validate before cache reuse or any preparation side effects.
        if !self
            .releases()?
            .iter()
            .any(|r| r.channel == channel && r.version == version)
        {
            bail!("this firmware is not in this RC2FX build's qualified catalog");
        }
        if stock {
            return self.prepare_stock(channel, version);
        }
        let mut saved = self.load()?;
        let key_path = if ssh {
            self.key.to_string_lossy().into_owned()
        } else {
            String::new()
        };
        let p = self
            .catalog()?
            .profiles
            .into_iter()
            .rfind(|p| p.model == self.model.id() && p.version == version)
            .filter(supported)
            .ok_or("this firmware is not supported by RC2FX")?;
        let (mut entries, runtime, settings) = self.image_for(&p, serial)?;
        // With SSH access the mixer also keeps one host key across flashes,
        // trusted under its address for the mixer tooling.
        let host_key = if ssh {
            let (extra, public) = crate::ssh::host_key(&self.work)?;
            entries.extend(extra);
            // Trusted under the address the developer tools use, if set.
            let host = self.source.config().remove("ip").unwrap_or_default();
            crate::ssh::remember_host(&self.work, &host, &public)?;
            files::sum(&public)
        } else {
            String::new()
        };
        if let Some(old) = saved.prepared.clone()
            && !old.stock
            && old.model.unwrap_or(Model::ProIi) == self.model
            && old.version == version
            && old.channel == channel
            && old.runtime == runtime
            && old.settings == settings
            && old.key.is_empty() != ssh
            && (!ssh || files::same_path(Path::new(&old.key), &self.key))
            && old.host_key == host_key
            && self.verify_prepared(&old).is_ok()
        {
            return Ok(old);
        }
        let selected = self
            .releases()?
            .into_iter()
            .find(|r| r.channel == channel && r.version == version)
            .ok_or("firmware is not in the qualified catalog")?;
        let md = self.get(&selected.digest_url, 128)?;
        if String::from_utf8_lossy(&md).trim().to_ascii_lowercase() != p.archive_md5 {
            bail!("RØDE's checksum differs from the inspected release");
        }
        let src = self.work.join("rode-original.tar.gz");
        self.download(&selected.url, &src, &p.archive_sha, &p.archive_md5)?;
        let key = if ssh {
            crate::ssh::ensure_key(&key_path)?.1
        } else {
            Vec::new()
        };
        let dst = self.work.join("archive.tar.gz");
        self.step("patch", "Building the firmware with RC2FX");
        if self.model == Model::Duo {
            // Shared executable hashes are NOT board identity. Verify the exact
            // Duo archive, hostname and DTB before adding anything.
            crate::inspection::inspect_archive(self.model, &src)?;
        }
        let (mix, launcher) = patch_archive_for_model(self.model, &src, &dst, &key, &entries)?;
        if mix != p.mixer_sha || launcher != p.launcher_sha {
            bail!("firmware binaries do not match the supported profile");
        }
        let (archive_sha, archive_md5) = {
            let check = self.reporter("verify", "Checking the built firmware");
            files::hash_both(&dst, check)?
        };
        let prepared = Prepared {
            model: Some(Model::parse(&p.model)?),
            version: p.version.clone(),
            channel: channel.into(),
            profile: p.id.clone(),
            archive_sha,
            archive_md5,
            key_sha: if ssh { files::sum(&key) } else { String::new() },
            key: key_path,
            host_key,
            runtime,
            settings,
            stock: false,
        };
        files::atomic_file(
            &self.work.join("archive.md5"),
            prepared.archive_md5.as_bytes(),
        )?;
        saved.prepared = Some(prepared.clone());
        self.save(&saved)?;
        Ok(prepared)
    }

    /// Download and verify a release and stage it unchanged: stock RØDE
    /// firmware, without RC2FX, its key or settings. Only qualified catalog
    /// releases are offered, with pinned SHA-256 and RØDE MD5 verification.
    fn prepare_stock(&mut self, channel: &str, version: &str) -> Result<Prepared> {
        let mut saved = self.load()?;
        if let Some(old) = saved.prepared.clone()
            && old.stock
            && old.model.unwrap_or(Model::ProIi) == self.model
            && old.version == version
            && old.channel == channel
            && self.verify_prepared(&old).is_ok()
        {
            return Ok(old);
        }
        let selected = self
            .releases()?
            .into_iter()
            .find(|r| r.channel == channel && r.version == version)
            .ok_or("firmware is not in the qualified catalog")?;
        let md = String::from_utf8_lossy(&self.get(&selected.digest_url, 128)?)
            .trim()
            .to_ascii_lowercase();
        if md.len() != 32 || !md.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("RØDE's checksum file is not an MD5");
        }
        let pinned = self
            .catalog()?
            .profiles
            .into_iter()
            .find(|p| p.model == self.model.id() && p.version == version && supported(p))
            .ok_or("stock firmware is not in the qualified catalog")?;
        if pinned.archive_md5 != md {
            bail!("RØDE's checksum differs from the inspected release");
        }
        let sha = &pinned.archive_sha;
        let src = self.work.join("rode-original.tar.gz");
        self.download(&selected.url, &src, sha, &md)?;
        if self.model == Model::Duo {
            crate::inspection::inspect_archive(self.model, &src)?;
        }
        let dst = self.work.join("archive.tar.gz");
        let part = self.work.join(".archive.tar.gz.part");
        match std::fs::remove_file(&part) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        {
            let copy = self.reporter("verify", "Staging the stock firmware");
            files::copy_with_progress(&src, &part, copy)?;
        }
        files::replace_local_file(&part, &dst)?;
        let (archive_sha, archive_md5) = {
            let check = self.reporter("verify", "Checking the stock firmware");
            files::hash_both(&dst, check)?
        };
        if archive_md5 != md || archive_sha != *sha {
            bail!("the staged stock firmware does not match RØDE's checksum");
        }
        let prepared = Prepared {
            model: Some(self.model),
            version: version.into(),
            channel: channel.into(),
            profile: pinned.id,
            archive_sha,
            archive_md5,
            stock: true,
            ..Default::default()
        };
        files::atomic_file(&self.work.join("archive.md5"), md.as_bytes())?;
        saved.prepared = Some(prepared.clone());
        self.save(&saved)?;
        Ok(prepared)
    }

    /// Check the prepared archive and its checksum file, and for RC2FX
    /// firmware the key it trusts and that its runtime is this installer's,
    /// before any USB command.
    pub(crate) fn verify_prepared(&mut self, p: &Prepared) -> Result<()> {
        // Missing is the legacy Pro II cache format, never automatic detection.
        let model = p.model.unwrap_or(Model::ProIi);
        model.require_runtime_with(self.allow_experimental_duo)?;
        let pinned = self.profile(&p.profile)?;
        if pinned.model != model.id() || pinned.version != p.version {
            bail!("prepared firmware does not match its qualified catalog profile");
        }
        if p.stock && (p.archive_sha != pinned.archive_sha || p.archive_md5 != pinned.archive_md5) {
            bail!("prepared stock firmware differs from its archive pins");
        }
        if model == Model::Duo {
            crate::inspection::verify_archive_model(model, &self.work.join("archive.tar.gz"))?;
        }
        if p.stock {
            let work = self.work.clone();
            let check = self.reporter("verify", "Checking the prepared firmware");
            return crate::usb::archive_matches(&work, p, check);
        }
        let profile = pinned;
        let bundle = self.source.bundle()?;
        let (_, runtime) = image_entries(&bundle, &profile, None)?;
        if p.runtime.is_empty() || p.runtime != runtime {
            bail!("the prepared firmware has a different RC2FX build; prepare it again");
        }
        if !p.key.is_empty() {
            let (_, public) = crate::ssh::ensure_key(&p.key)?;
            if files::sum(&public) != p.key_sha {
                bail!(
                    "the SSH key differs from the one in the prepared firmware; prepare it again"
                );
            }
        }
        let work = self.work.clone();
        let check = self.reporter("verify", "Checking the prepared firmware");
        crate::usb::archive_matches(&work, p, check)
    }
}
