//go:build !embedded

package embed

const Present = false

const Missing = "this build does not embed the installer payload; build it with the `embedded` build tag"

func Blob() []byte { return nil }
