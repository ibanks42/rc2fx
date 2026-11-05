# RC2FX mixer software notes

Curated notes for independent RC2FX mixer releases and historical desktop
app bundles. This file is the source for the mixer history page and the
`mixer.releases` array in `updates.json`; the app history comes from
`CHANGELOG.md`.

- Add a section when preparing a mixer release; it appears in the history only
  after the `mixer-vVERSION` release is published.
- Start the heading with the exact `MAJOR.MINOR.PATCH` version from
  `config/mixer-release.json`, without a `v`.
- Leave a version out when you don't know what changed: the site and the feed
  say the history is unknown instead of guessing.
- Publication dates and historical app bundles are derived from the release
  history, so don't repeat them here.
- There is one section per mixer version, independent of app releases.
- This feed provides release notes only. Installation compatibility and payload
  hashes come from the separately signed update repository.

## [1.0.1]

- Reduce audio deadline interference from medium-sized mixer screen redraws.
- Correct suppression timing totals after changing lookahead settings.
- Preserve native performance samples and recent audio faults in diagnostics.

## [1.0.0]

- First published RC2FX mixer software, bundled with RC2FX 1.0.0.
- Processes audio for the RØDECaster Pro II and applies the app's settings.
- Reports its software version and control protocol to the app.
