//go:build embedded

package embed

import (
	_ "embed"
	"fmt"
)

// The payload, staged by cmd/stage-payload with every check a folder's
// payload passes. A missing file fails the build: go:embed needs it.
//
//go:embed payload/rc2fx-payload.bin
var blob []byte

// Present reports whether this build embeds the payload.
const Present = true

// Missing says why there is no payload; empty when there is one.
const Missing = ""

// Blob is the embedded envelope.
func Blob() []byte { return blob }

// A damaged payload fails the start: the desktop app installs RC2FX from
// it, so there is no fallback to files beside it.
func init() {
	envelope, err := Find(blob)
	if err == nil {
		_, err = envelope.Bundle()
	}
	if err != nil {
		panic(fmt.Sprintf("cannot use the embedded installer payload: %v", err))
	}
}
