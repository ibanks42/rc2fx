# RC2FX mixer software notes

Curated notes for the RC2FX mixer software versions published inside desktop
app releases. This file is the source for the mixer history page and the
`mixer.releases` array in `updates.json`; the app history comes from
`CHANGELOG.md`.

- Add a section only when a version is bundled by a desktop release.
- Start the heading with the exact `MAJOR.MINOR.PATCH` version from
  `config/mixer-release.json`, without a `v`.
- Leave a version out when you don't know what changed: the site and the feed
  say the history is unknown instead of guessing.
- The first app version that bundled a mixer version and its publication date
  are derived from the app release history, so don't repeat them here.
- The mixer is not released on its own; there is one section per published
  mixer version, not per app release.

## [1.0.1]

- Reduce audio deadline interference from medium-sized mixer screen redraws.
- Correct suppression timing totals after changing lookahead settings.
- Preserve native performance samples and recent audio faults in diagnostics.

## [1.0.0]

- First published RC2FX mixer software, bundled with RC2FX 1.0.0.
- Processes audio for the RØDECaster Pro II and applies the app's settings.
- Reports its software version and control protocol to the app.
