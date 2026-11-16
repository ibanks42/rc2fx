//go:build !embedded

package embed

// Present reports whether this build embeds the payload.
const Present = false

// Missing says why there is no payload.
const Missing = "this build does not embed the installer payload; build it with the `embedded` build tag"

// Blob is the embedded envelope: empty without the `embedded` tag.
func Blob() []byte { return nil }
