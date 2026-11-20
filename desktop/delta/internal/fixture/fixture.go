// Package fixture generates test inputs for the delta fixtures. Large() is
// duplicated in crates/rc2fx-delta/tests/shared_fixtures.rs; keep both in sync.
package fixture

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
)

type File struct {
	Name string
	Data []byte
}

func Sum(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}

// Envelope builds an RC2FX-EMBEDDED-PAYLOAD 1 envelope from files.
func Envelope(files []File) []byte {
	var body []byte
	for _, f := range files {
		body = fmt.Appendf(body, "%s  %d  %s\n", Sum(f.Data), len(f.Data), f.Name)
	}
	for _, f := range files {
		body = append(body, f.Data...)
	}
	out := []byte("RC2FX-EMBEDDED-PAYLOAD 1\n")
	out = fmt.Appendf(out, "length %d\n", len(body))
	out = fmt.Appendf(out, "sha256 %s\n", Sum(body))
	return append(out, body...)
}

// Sample returns the standard test inputs shared with the Rust tests: an
// unchanged file, an old/new pair for patching, and a compressible file.
func Sample() (shared, old, updated, gui []byte) {
	shared = bytes.Repeat([]byte("a file the mixer already has\n"), 20)
	old = bytes.Repeat([]byte("the previous rc2fxd build\n"), 200)
	updated = bytes.Clone(old)
	updated[7] ^= 1
	updated = append(updated, bytes.Repeat([]byte("and its new tail\n"), 20)...)
	gui = bytes.Repeat([]byte("compressible gui bytes "), 2000)
	return
}

// Random generates n deterministic pseudo-random bytes using xorshift64
// (13, 7, 17).
func Random(seed uint64, n int) []byte {
	out := make([]byte, n)
	x := seed
	for i := range out {
		x ^= x << 13
		x ^= x >> 7
		x ^= x << 17
		out[i] = byte(x)
	}
	return out
}

// Text generates n bytes of compressible pseudo-random text.
func Text(seed uint64, n int) []byte {
	words := []string{"mixer ", "route ", "input ", "gain ", "level ", "rc2fx ", "\n", "0x7f ", "fader "}
	picks := Random(seed, n)
	var out []byte
	for i := 0; len(out) < n; i++ {
		out = append(out, words[int(picks[i])%len(words)]...)
	}
	return out[:n]
}

// Large constructs a 1.2 MB patch fixture whose matches reach ~1.2 MB back
// into the prefix (requiring a 4 MiB window), paired with an unchanged 4 KB
// file.
func Large() (envelope []byte, installed map[string][]byte) {
	base := Random(0x9e3779b97f4a7c15, 1_200_000)
	updated := Random(2, 1000)
	changed := bytes.Clone(base)
	for i := 50_000; i < len(changed); i += 100_000 {
		changed[i] ^= 0xff
	}
	updated = append(updated, changed...)
	updated = append(updated, Random(3, 10_000)...)
	shared := Random(4, 4096)
	envelope = Envelope([]File{{Name: "shared", Data: shared}, {Name: "rc2fxd", Data: updated}})
	return envelope, map[string][]byte{"shared": shared, "rc2fxd": base}
}
