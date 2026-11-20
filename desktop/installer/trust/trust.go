// Package trust embeds config/mixer-update-root.json (kept in sync by tests)
// for both the mixer and app TUF repos, since go:embed can't cross module roots.
package trust

import (
	"bytes"
	_ "embed"
)

//go:embed mixer-update-root.json
var root []byte

// Root returns a copy of the embedded TUF root metadata.
func Root() []byte { return bytes.Clone(root) }
