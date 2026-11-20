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
	"bytes"
	"errors"
	"sort"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Canonical encodes value as OLPC canonical JSON (olpc-cjson): object keys
// sorted by encoded bytes, only `"` and `\` escaped, and no floats.
func Canonical(value core.Value) ([]byte, error) {
	return appendCanonical(nil, value)
}

func appendCanonical(dst []byte, value core.Value) ([]byte, error) {
	switch value.Kind() {
	case core.ValueNull:
		return append(dst, "null"...), nil
	case core.ValueBool:
		b, _ := value.AsBool()
		if b {
			return append(dst, "true"...), nil
		}
		return append(dst, "false"...), nil
	case core.ValueNumber:
		n, _ := value.AsNumber()
		if n.IsF64() {
			return nil, errors.New("floating point numbers are not allowed in canonical JSON")
		}
		return append(dst, n.String()...), nil
	case core.ValueString:
		text, _ := value.AsString()
		return appendCanonicalString(dst, text), nil
	case core.ValueArray:
		items, _ := value.AsArray()
		dst = append(dst, '[')
		for i, item := range items {
			if i > 0 {
				dst = append(dst, ',')
			}
			var err error
			if dst, err = appendCanonical(dst, item); err != nil {
				return nil, err
			}
		}
		return append(dst, ']'), nil
	case core.ValueObject:
		members, _ := value.AsObject()
		type entry struct {
			key   []byte
			value core.Value
		}
		entries := make([]entry, len(members))
		for i, m := range members {
			entries[i] = entry{appendCanonicalString(nil, m.Key), m.Value}
		}
		sort.SliceStable(entries, func(i, j int) bool { return bytes.Compare(entries[i].key, entries[j].key) < 0 })
		dst = append(dst, '{')
		for i, e := range entries {
			if i > 0 {
				dst = append(dst, ',')
			}
			dst = append(dst, e.key...)
			dst = append(dst, ':')
			var err error
			if dst, err = appendCanonical(dst, e.value); err != nil {
				return nil, err
			}
		}
		return append(dst, '}'), nil
	}
	return nil, errors.New("unknown JSON value")
}

func appendCanonicalString(dst []byte, text string) []byte {
	dst = append(dst, '"')
	for i := 0; i < len(text); i++ {
		switch c := text[i]; c {
		case '"', '\\':
			dst = append(dst, '\\', c)
		default:
			dst = append(dst, c)
		}
	}
	return append(dst, '"')
}

// Pretty formats value as 2-space indented JSON with a trailing newline.
func Pretty(value core.Value) []byte {
	return append([]byte(value.Pretty()), '\n')
}
