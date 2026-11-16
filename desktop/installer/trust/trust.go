// Package trust holds the trusted root both signed repositories share: a
// copy of config/mixer-update-root.json (a test keeps the two identical),
// embedded here because go:embed cannot reach outside this module.
package trust

import (
	"bytes"
	_ "embed"
)

//go:embed mixer-update-root.json
var root []byte

// Root is the trusted root for mixer and app releases.
func Root() []byte { return bytes.Clone(root) }
