//go:build linux

package installer

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"syscall"
)

// helperDirectory is name under `/var/lib/rc2fx` when running as the
// privileged Linux helper: state the desktop user cannot change. Empty
// everywhere else.
func helperDirectory(name string) (string, error) {
	if os.Geteuid() != 0 {
		return "", nil
	}
	path := "/"
	for _, part := range []string{"var", "lib", "rc2fx", name} {
		path = filepath.Join(path, part)
		_, err := os.Lstat(path)
		if errors.Is(err, fs.ErrNotExist) {
			if err := os.Mkdir(path, 0o700); err != nil {
				return "", err
			}
		} else if err != nil {
			return "", err
		}
		info, err := os.Lstat(path)
		if err != nil {
			return "", err
		}
		stat, ok := info.Sys().(*syscall.Stat_t)
		if !info.IsDir() || !ok || stat.Uid != 0 || uint32(info.Mode().Perm())&0o022 != 0 {
			return "", mixerError("helper metadata directory must be a root-owned directory, not writable by other users")
		}
	}
	return path, nil
}
