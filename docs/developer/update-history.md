# Update histories and the feed

The documentation site publishes the release history of both versioned parts
of RC2FX and a JSON feed the app reads:

- `/updates/app` lists every published desktop app release and its notes.
- `/updates/mixer` lists published standalone mixer releases and historical
  app bundles.
- `/updates.json` carries the same release-note history. Signed TUF metadata,
  described in [Mixer releases](mixer-releases.md), authorizes installation.

All three are generated at documentation build time by
`.github/scripts/update_history.py`. They are not committed, so they cannot
go stale between a source change and a build.

## Sources

| Source | Supplies |
| --- | --- |
| `CHANGELOG.md` | App notes, one section per stable app version. git-cliff writes it during release preparation. |
| `docs/updates/source/mixer-notes.md` | Curated mixer notes, one section per published mixer version. |
| `config/mixer-release.json` at each release tag | Which mixer software an app release bundled, and its control protocol version. |
| GitHub's releases API | Which app and standalone mixer releases are actually published. Drafts and pre-releases are excluded even when their tag and changelog section already exist. |

Nothing is written twice. The app notes are the changelog sections, not a
copy; the mixer notes are authored once and used by both the page and the
feed.

## Adding mixer notes

When preparing a new `config/mixer-release.json` version, follow
[Mixer releases](mixer-releases.md),
then add a section to `docs/updates/source/mixer-notes.md`:

```markdown
## [1.1.0]

- What changed for the person using the mixer.
```

Use the exact version, without a `v`. Publication dates and historical app
bundles are derived from the release history, so don't repeat them. If you don't know what changed, leave the section out: the page
says the notes are unavailable and the feed uses `notesMarkdown: null` rather
than inventing history. Review the notes when publishing a mixer release; missing notes stay
explicitly unknown.

The desktop app's notes come from commit messages through git-cliff. See
[Release notes](releases.md#release-notes) for the commit prefixes and
wording.

## The app to mixer mapping

Download-only app tags are identified by the optional `embedded` feature
being absent from their default features in `crates/rc2fx-app/Cargo.toml`.
They set `downloadsMixer: true`, have no bundled version, and do not make
mixer history incomplete. Standalone releases come from `mixer-vVERSION` tags.

There is no separate mapping file. An older app release's bundled mixer software
comes from `config/mixer-release.json` at that release's tag, read with:

```sh
git show refs/tags/vX.Y.Z:config/mixer-release.json
```

The documentation checkout must therefore contain the release tags. A
published release whose tag is missing fails the build rather than turning
the mixer history silently unknown. A tag that predates the manifest is
explicit unknown. The generator never reads the working tree's manifest; release
preparation reads it separately when validating the pending release.

`protocolVersion` may be `null` when unknown. `mixerVersion` is never guessed
from the app version or from version gaps.

## Abandoned releases

A release can be prepared and then never published. Its changelog section
still exists, and the next published release above it includes those changes.
The generator folds sections for unpublished versions between two published
releases into the next published release, with a line saying which version
the notes were prepared as:

```markdown
**Prepared as v1.1.0, not published.**
```

Sections above the newest published release stay hidden. A published release
without its own section gets `notesMarkdown: null` if an older release, but
the newest published release must have notes or the build fails. Unshipped
mixer notes between two shipped mixer versions are folded into the next
shipped version the same way; one that was never shipped above the newest
version stays out.

## Published releases only

The generator asks the GitHub releases API for every page of releases and
keeps only non-draft, non-prerelease `vMAJOR.MINOR.PATCH` app tags and
`mixer-vMAJOR.MINOR.PATCH` mixer tags. Tags and
changelog sections are not proof of publication: the release workflow creates
the tag and the draft before macOS finishes. A manual documentation build
from `main` therefore cannot publish notes for an unfinished draft.

The build fails when one release's notes exceed 64 KiB or the feed exceeds
4 MiB, the limits the app reads.

The API token comes from `GH_TOKEN` or `GITHUB_TOKEN` when set. Without one,
the request is unauthenticated and subject to GitHub's rate limit.

## Release preflight

Release preparation validates the pending release directly:
`release.py prepare` parses the new app section with `parse_changelog` and
does not require mixer changes or notes for download-only app releases.
Legacy embedded release preparation also validates changed mixer notes. The release workflow then runs the ordinary published-only
documentation build (`npm run build`), so a prepared but unpublished release
never enters the site or `updates.json`. There is no synthetic candidate and
no candidate output to keep or clean up.

## Feed contract

```json
{
  "schemaVersion": 1,
  "app": {
    "latest": "1.0.0",
    "historyStart": "1.0.0",
    "historyComplete": true,
    "releases": [
      {
        "version": "1.0.0",
        "date": "2026-11-04",
        "notesMarkdown": "### Features\n\n- ...",
        "releaseUrl": "https://github.com/ibanks42/rc2fx/releases/tag/v1.0.0",
        "mixerVersion": "1.0.0",
        "protocolVersion": 1
      }
    ]
  },
  "mixer": {
    "latest": "1.0.0",
    "historyStart": "1.0.0",
    "historyComplete": true,
    "releases": [
      {
        "version": "1.0.0",
        "date": "2026-11-04",
        "notesMarkdown": "- ...",
        "firstAppVersion": "1.0.0"
      }
    ]
  }
}
```

- `releases` are newest first and contain published versions only.
- `latest` and `historyStart` are explicit. `historyStart` is the oldest
  version the published history covers; it is not inferred from the gaps
  between entries. Both are `null` when nothing is published.
- `historyComplete` is `false` when any part of the history is unknown: an
  older app release without notes, an older bundled app release without a recorded mixer
  version, or a mixer version without notes. A known first and last entry
  alone never proves the history between them is complete.
- `notesMarkdown` is Markdown, rendered as formatted text. `null` means the
  history is unknown; it is never an empty list pretending there were no
  changes. The newest published app entry always has notes; older entries may
  not.
- `mixerVersion`, `protocolVersion`, `date` and `firstAppVersion` may be
  `null` when unknown.
- `firstAppVersion` is the oldest published app release that bundled the
  mixer version, or `null` for a standalone version never bundled by an app.
- `downloadsMixer` is true for download-only app releases. Older feed entries
  without this field retain their historical bundled-version semantics.
- New fields may be added without changing `schemaVersion`; the app ignores
  fields it doesn't know. Removing or reinterpreting a field needs a new feed
  URL so older apps keep working.

## Testing and building

```sh
python3 .github/scripts/test_update_history.py   # offline generator tests
just docs                                        # generate and build the site
```

The tests inject a captured GitHub release list and mapping, so they need no
network and no tags. The documentation build needs the API (or `GH_TOKEN`)
and the release tags because it decides what is published and what each app
bundled.

`.github/scripts/update_history.py --published-json FILE` builds from a saved
API reply instead of fetching one, which is useful when working offline.
