package installer

// The flashing library's tar names the tests here build fixtures with.

import "github.com/ibanks42/rc2fx/desktop/rodecaster/archive"

type (
	tarHeader = archive.Header
	tarWriter = archive.Writer
)

const (
	typeRegular  = archive.TypeRegular
	typeLongName = archive.TypeLongName
	typePaxLocal = archive.TypePaxLocal
)

func octal(field []byte, v uint64)            { archive.Octal(field, v) }
func numeric(field []byte) (uint64, error)    { return archive.Numeric(field) }
func archivePath(path string) (string, error) { return archive.ArchivePath(path) }
func copyWithProgress(src, dst string, progress func(uint64, uint64)) error {
	return archive.CopyWithProgress(src, dst, progress)
}
