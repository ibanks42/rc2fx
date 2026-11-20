# Updates and release history

RC2FX has two separately versioned parts: the desktop app and the RC2FX mixer
software. Mixer software is published as its own signed release, which the app
downloads when it installs or updates RC2FX. Both histories are published here,
newest first, with the notes from the same sources the app reads.

- [App release history](app.md) lists every published desktop app release.
  Older app releases bundled the mixer software, and their entries name the
  version they carried.
- [Mixer software history](mixer.md) lists every mixer software version,
  whether published on its own or inside an older app release.

The app shows only the notes between the version you have installed and the
version being offered. For the mixer, the app compares the version the mixer
reports with the signed mixer releases it can install; each release names the
oldest app version that can install it.

The same data is available as a JSON feed at
[`/updates.json`](/updates.json) for update checks. The feed lists
published releases only.

RØDE firmware versions are published by RØDE and are not part of these
histories. See [Firmware support](../reference/firmware.md) for the versions
RC2FX supports.
