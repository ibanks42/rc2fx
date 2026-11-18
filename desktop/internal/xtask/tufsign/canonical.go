// Package tufsign signs TUF metadata exactly as the tough 0.24 crate does,
// so the mixer's sigstore-tuf client and the desktop's verifier accept what
// it writes:
//
//   - roles are signed over OLPC canonical JSON of their `signed` object
//     (keys sorted by their bytes, only `"` and `\` escaped, no floats);
//   - files are serde_json's pretty form (two-space indentation) and a final
//     newline, in tough's field order;
//   - `expires` has 0, 3, 6 or 9 fractional digits, the fewest that are
//     exact (tough's serialize_timestamp);
//   - keys are tough's: RSA as SubjectPublicKeyInfo PEM with RSA-PSS/SHA-256,
//     Ed25519 as hex, ECDSA P-256 as PEM; a key ID is the SHA-256 of the
//     key's canonical JSON;
//   - a repository is `N.root.json` (the root file as given),
//     `targets.json`, `snapshot.json` and `timestamp.json`, with snapshot and
//     timestamp pins of version, length and SHA-256.
//
// Documents are core.Value trees, so fields tough keeps but does not know
// (its `_extra`) survive a renewal.
package tufsign

import (
	"bytes"
	"errors"
	"sort"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Canonical is value as OLPC canonical JSON (olpc-cjson, which tough uses):
// object members sorted by their encoded keys, strings with only `"` and `\`
// escaped and every other character as is, integers only.
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

// Pretty is serde_json's to_vec_pretty and the newline tough adds: the bytes
// of a metadata file.
func Pretty(value core.Value) []byte {
	return append([]byte(value.Pretty()), '\n')
}
