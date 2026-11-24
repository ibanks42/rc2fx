// Package tufsign produces TUF metadata matching the tough 0.24 Rust crate so
// both the mixer's sigstore-tuf client and the desktop verifier accept it.
//
// Wire details kept compatible with tough:
//   - Signatures cover OLPC canonical JSON of `signed` (byte-sorted keys, only
//     `"` and `\` escaped, integers only).
//   - Files are 2-space pretty-printed JSON with a trailing newline and
//     tough's field ordering.
//   - `expires` uses 0, 3, 6, or 9 fractional digits (shortest exact group).
//   - RSA keys use SPKI PEM + RSA-PSS/SHA-256, Ed25519 uses hex, ECDSA P-256
//     uses PEM; key IDs are SHA-256 of canonical key JSON.
//   - Documents are kept as core.Value trees so unknown `_extra` fields survive
//     renewals.
package tufsign

import (
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/tuf"
)

// Canonical encodes value as OLPC canonical JSON (olpc-cjson): object keys
// sorted by encoded bytes, only `"` and `\` escaped, and no floats.
func Canonical(value core.Value) ([]byte, error) {
	return tuf.Canonical(value)
}

// Pretty formats value as 2-space indented JSON with a trailing newline.
func Pretty(value core.Value) []byte {
	return append([]byte(value.Pretty()), '\n')
}
