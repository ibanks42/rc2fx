# Update histories and the feed

The docs site at `https://rc2fx.ibanks.dev` publishes release histories for
both versioned halves of RC2FX, plus a JSON feed the desktop app consumes:

- `/updates/app`: every published desktop app release and its release notes.
- `/updates/mixer`: published standalone mixer releases along with historical
  app-bundled versions.
- `/updates.json` (`https://rc2fx.ibanks.dev/updates.json`): the same
  release-note history in JSON. The app fetches the custom domain directly
  because update checks never follow redirects. Installation itself is still
  authorized by signed TUF metadata (see [Mixer releases](mixer-releases.md)).

`.github/scripts/update_history.py` generates all three during the docs build.
None of them are checked into git, so they can't go stale between edits.

## Sources

| Source | Supplies |
| --- | --- |
| `CHANGELOG.md` | App release notes, one section per stable app version (written by git-cliff during release prep). |
| `docs/updates/source/mixer-notes.md` | Hand-written mixer notes, one section per published mixer version. |
| `config/mixer-release.json` at each release tag | Which mixer runtime an older app release bundled and its control protocol version. |
| GitHub's releases API | Which app and standalone mixer releases are actually published. Drafts and pre-releases are ignored even if a tag and changelog entry already exist. |

Nothing is duplicated: app notes come straight from `CHANGELOG.md`, and mixer
notes are written once and rendered into both the web page and the JSON feed.

## Adding mixer notes

When you're preparing a new `config/mixer-release.json` version, follow
[Mixer releases](mixer-releases.md) and add an entry to
`docs/updates/source/mixer-notes.md`:

```markdown
## [1.1.0]

- What changed for the person using the mixer.
```

Use the bare version number with no `v` prefix. Don't add publication dates or
historical app bundles, since those are derived from the release history. If you
genuinely don't know what changed, leave the section out; the page will say
notes are unavailable and the feed will emit `notesMarkdown: null` instead of
making something up. Review these notes before publishing a mixer release, and
let missing notes stay explicitly unknown.

App release notes are generated from commit messages by git-cliff. See
[Release notes](releases.md#release-notes) for the commit prefixes and style.

## The app to mixer mapping

Download-only app tags are any tag carrying the Go desktop app
(`desktop/go.mod`), whose release packages always download the mixer runtime,
plus older Rust-app tags where `crates/rc2fx-app/Cargo.toml` didn't include
`embedded` in its default features. Those entries set `downloadsMixer: true`,
have no bundled mixer version, and don't mark mixer history as incomplete.
Standalone mixer releases come from `mixer-vVERSION` tags.

There's no separate mapping file. For older bundled app releases, the generator
reads `config/mixer-release.json` directly from the release's git tag:

```sh
git show refs/tags/vX.Y.Z:config/mixer-release.json
```

Because of that, the checkout building the docs has to have the release tags
fetched. If a published release's tag is missing, the build fails instead of
silently marking the mixer history unknown. A tag from before the manifest
existed is marked explicitly unknown. The generator never looks at the working
tree's copy of the manifest. Release prep checks that separately when
validating a pending release.

`protocolVersion` can be `null` when unknown, and `mixerVersion` is never
guessed from the app version or version gaps.

## Abandoned releases

Sometimes a release gets prepared and tagged, then abandoned before
publication. Its changelog section stays in `CHANGELOG.md`, and the next
published release picks up those commits. The generator folds any unpublished
sections sitting between two published releases into the next published one,
prefixed with:

```markdown
**Prepared as v1.1.0, not published.**
```

Any sections newer than the latest published release stay hidden. An older
published release with no changelog section gets `notesMarkdown: null`, while
the newest published release must have notes or the build fails. Unshipped
mixer notes between two shipped mixer versions fold into the next shipped
version the same way, and anything newer than the latest shipped version is
left out.

## Published releases only

The generator pages through GitHub's releases API and keeps only non-draft,
non-prerelease `vMAJOR.MINOR.PATCH` app tags and `mixer-vMAJOR.MINOR.PATCH`
mixer tags. Having a git tag or changelog entry doesn't mean a release is
published. The release workflow creates the tag and draft before the macOS
build finishes, so a manual docs build from `main` won't leak notes for an
in-progress draft.

The build also enforces the app's size limits: it fails if any single release's
notes exceed 64 KiB or if the whole feed exceeds 4 MiB.

Set `GH_TOKEN` or `GITHUB_TOKEN` to authenticate API requests; without one,
GitHub's unauthenticated rate limits apply.

## Release preflight

Release preparation validates the pending app release on its own:
`release.py prepare` parses the new app section with `parse_changelog` and
doesn't expect mixer changes or mixer notes, since app releases don't bundle
the mixer runtime. The workflow then runs the normal published-only docs build
(`npm run build`), so a prepared-but-unpublished release never enters the site
or `updates.json`.

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
        "firstAppVersion": "1.0.0",
        "standalone": false
      }
    ]
  }
}
```

- `releases` is ordered newest first and includes published versions only.
- `latest` and `historyStart` are explicit (`historyStart` is the oldest
  version covered by published history, never inferred from gaps). Both are
  `null` if nothing has been published.
- `historyComplete` drops to `false` if any piece of history is missing: an
  older app release without notes, an older bundled app release without a
  recorded mixer version, or a mixer version without notes. Knowing the first
  and last entries doesn't guarantee the middle is complete.
- `notesMarkdown` is Markdown rendered as formatted text. `null` means unknown
  (never an empty list pretending nothing changed). The latest published app
  entry always has notes; older ones might not.
- `mixerVersion`, `protocolVersion`, `date`, and `firstAppVersion` can be
  `null` when unknown.
- `firstAppVersion` is the earliest published app release that bundled that
  mixer version, or `null` for standalone mixer releases that were never
  bundled.
- `standalone` is `true` when that mixer version was published as its own
  `mixer-v` release. The desktop app treats the newest standalone release as
  the latest published mixer software and links its notes to that `mixer-v`
  release page when it wasn't bundled in an earlier app.
- `downloadsMixer` is `true` for download-only app releases. Older entries
  without this field keep their original bundled-version meaning.
- Adding new fields doesn't require bumping `schemaVersion` since the app
  ignores unknown fields. Removing or changing the meaning of an existing field
  requires a new feed URL so older apps don't break.

## Testing and building

```sh
python3 .github/scripts/test_update_history.py   # offline generator tests
just docs                                        # generate and build the site
```

The unit tests feed in a mocked GitHub release list and tag map, so they run
offline. Building the real docs site does need API
access (or `GH_TOKEN`) and the release tags so it knows what's published and
what each older app bundled.

For offline work, `.github/scripts/update_history.py --published-json FILE`
reads a saved API response instead of hitting GitHub.
