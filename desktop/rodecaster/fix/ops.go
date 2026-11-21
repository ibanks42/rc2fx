package fix

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

// now is replaceable in tests so backup names are predictable.
var now = time.Now

func backupName(prefix string, sector0 []byte, t time.Time) string {
	id := binary.LittleEndian.Uint32(sector0[440:444])
	return fmt.Sprintf("%s%08x-%s.bin", prefix, id, t.UTC().Format("20060102T150405Z"))
}

// writeBackup saves sector0 in dir under a new name, durably, and never over
// an existing file.
func writeBackup(dir, prefix string, sector0 []byte) (string, error) {
	if dir == "" {
		return "", errors.New("no backup folder was given")
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	path := filepath.Join(dir, backupName(prefix, sector0, now()))
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return "", err
	}
	_, err = f.Write(sector0)
	if err == nil {
		err = f.Sync()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = syncDir(dir)
	}
	if err != nil {
		os.Remove(path)
		return "", err
	}
	return path, nil
}

// syncDir makes the new file's directory entry durable. Windows cannot
// flush a directory handle; NTFS journals the entry itself.
func syncDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	f, err := os.Open(dir)
	if err != nil {
		return err
	}
	err = f.Sync()
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

// SaveSector0 saves a copy of a 512-byte sector 0 in dir under the same kind
// of name a fix gives its backup, so `restore` takes it.
func SaveSector0(dir string, sector0 []byte) (string, error) {
	if len(sector0) != 512 {
		return "", fmt.Errorf("a sector 0 has 512 bytes, not %d", len(sector0))
	}
	return writeBackup(dir, "rcfix-sector0-", sector0)
}
