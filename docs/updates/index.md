# Updates and release history

RC2FX ships as two independently versioned parts: the desktop app and the RC2FX
mixer software. The mixer software is published as its own signed release,
which the desktop app downloads when installing or updating RC2FX. Both
histories live here, newest first, pulled from the same sources the app reads.

- [App release history](app.md) lists every published desktop app release.
  Older app releases bundled the mixer software, and those entries note which
  mixer version they carried.
- [Mixer software history](mixer.md) lists every mixer software version,
  whether released on its own or bundled inside an older app release.

In the app, you only see release notes for versions between what you have
installed and what's being offered. For the mixer, the app compares the version
the mixer reports against the signed mixer releases it can install; each
release specifies the oldest desktop app version that can install it.

The same information is exposed as a JSON feed at
[`https://rc2fx.ibanks.dev/updates.json`](/updates.json) for update checks.
Only published releases appear in the feed.

RØDE firmware versions are published by RØDE and aren't tracked in these
histories. See [Firmware support](../reference/firmware.md) for the versions
RC2FX supports.
