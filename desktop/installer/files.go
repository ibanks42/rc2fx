package installer

// Helpers for private local files (journals, keys, downloads, backups).

import (
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

func privateDirs(dir string) error {
	info, err := os.Stat(dir)
	switch {
	case err == nil && info.IsDir():
		return nil
	case err == nil:
		return errorf("mkdir %s: not a directory", dir)
	case !errors.Is(err, fs.ErrNotExist):
		return err
	}
	if parent := filepath.Dir(dir); parent != dir && parent != "." && parent != "" {
		if err := privateDirs(parent); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o700); err != nil {
		if errors.Is(err, fs.ErrExist) {
			return privateDirs(dir)
		}
		return err
	}
	file, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer file.Close()
	return archive.LocalOwner(file)
}

func openLocalFile(path string, appendMode bool) (*os.File, error) {
	flags := os.O_WRONLY
	if appendMode {
		flags |= os.O_APPEND
	}
	file, err := os.OpenFile(path, flags|os.O_CREATE|os.O_EXCL, 0o600)
	if err == nil {
		if err := archive.LocalOwner(file); err != nil {
			file.Close()
			return nil, err
		}
		return file, nil
	}
	if !errors.Is(err, fs.ErrExist) {
		return nil, err
	}
	file, err = os.OpenFile(path, flags|archive.NoFollow, 0o600)
	if err != nil {
		return nil, err
	}
	if file, err = archive.RegularOnly(file); err != nil {
		return nil, err
	}
	return archive.OwnedByInvoker(file)
}

func exclusiveFile(path string, data []byte) error {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	if err := archive.LocalOwner(file); err != nil {
		return err
	}
	if _, err := file.Write(data); err != nil {
		return err
	}
	return file.Sync()
}

func readLimited(r io.Reader, limit int64) ([]byte, error) {
	return io.ReadAll(io.LimitReader(r, limit))
}
