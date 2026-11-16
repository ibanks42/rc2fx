//go:build !cgo

package ext2fs

// Without cgo there is no libext2fs: everything reports that.

var unavailable = invalid("this build has no libext2fs (built without cgo)")

// Filesystem is an open ext4 filesystem; none can be opened in this build.
type Filesystem struct{}

// Open reports that this build has no libext2fs.
func Open(device string, offset uint64, writable bool) (*Filesystem, error) {
	return nil, unavailable
}

func (f *Filesystem) NeedsRecovery() bool { return false }

func (f *Filesystem) Lookup(path string) (*Entry, error) { return nil, unavailable }

func (f *Filesystem) Read(entry Entry) ([]byte, error) { return nil, unavailable }

func (f *Filesystem) Overwrite(entry Entry, data []byte) error { return unavailable }

func (f *Filesystem) Close() error { return nil }
