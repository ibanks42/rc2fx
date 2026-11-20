//go:build embedded

package embed

import (
	_ "embed"
	"fmt"
)

// Staged by cmd/stage-payload before building with -tags embedded.
//
//go:embed payload/rc2fx-payload.bin
var blob []byte

const Present = true

const Missing = ""

func Blob() []byte { return blob }

// Fail fast at startup if the embedded payload is corrupt.
func init() {
	envelope, err := Find(blob)
	if err == nil {
		_, err = envelope.Bundle()
	}
	if err != nil {
		panic(fmt.Sprintf("cannot use the embedded installer payload: %v", err))
	}
}
