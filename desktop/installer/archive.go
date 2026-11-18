package installer

// RC2FX's names for the flashing library's archive and private-file
// helpers, which many call sites here use.

import (
	"os"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

type (
	// Entry is a file or directory to add to the firmware image.
	Entry     = archive.Entry
	EntryKind = archive.EntryKind
	tarEntry  = archive.Member
	algorithm = archive.Algorithm
	patched   = archive.Patched
)

const (
	EntryDir        = archive.EntryDir
	EntryFile       = archive.EntryFile
	maxMixerMember  = archive.MaxMixerMember
	maxSmallMember  = archive.MaxSmallMember
	algorithmSha256 = archive.SHA256
	algorithmMd5    = archive.MD5
)

func sum(data []byte) string                            { return archive.Sum(data) }
func normalized(name string) string                     { return archive.Normalized(name) }
func trimASCII(data []byte) []byte                      { return archive.TrimASCII(data) }
func readAll(e *tarEntry) ([]byte, error)               { return archive.ReadAll(e) }
func openRegular(path string) (*os.File, error)         { return archive.OpenRegular(path) }
func openWorkFile(path string) (*os.File, error)        { return archive.OpenWorkFile(path) }
func atomicFile(path string, data []byte) error         { return archive.AtomicFile(path, data) }
func localOwner(file *os.File) error                    { return archive.LocalOwner(file) }
func ownedByInvoker(f *os.File) (*os.File, error)       { return archive.OwnedByInvoker(f) }
func fileHash(path string, a algorithm) (string, error) { return archive.FileHash(path, a) }

func walkArchive(path string, limit uint64, raw bool, visit func(*tarEntry) error) error {
	return archive.Walk(path, limit, raw, visit)
}

func hashBoth(path string, progress func(uint64, uint64)) (string, string, error) {
	return archive.HashBoth(path, progress)
}
