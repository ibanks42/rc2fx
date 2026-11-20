//go:build !cgo

package ext2fs

// Stub implementation used when built without cgo.

var unavailable = invalid("this build has no libext2fs (built without cgo)")

// Filesystem is a stub when built without cgo.
type Filesystem struct{}

// Open always fails when built without cgo.
func Open(device string, offset uint64, writable bool) (*Filesystem, error) {
	return nil, unavailable
}

func (f *Filesystem) NeedsRecovery() bool { return false }

func (f *Filesystem) Lookup(path string) (*Entry, error) { return nil, unavailable }

func (f *Filesystem) Read(entry Entry) ([]byte, error) { return nil, unavailable }

func (f *Filesystem) Overwrite(entry Entry, data []byte) error { return unavailable }

func (f *Filesystem) Close() error { return nil }
