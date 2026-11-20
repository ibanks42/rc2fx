package installer

// The record that ties a prepared RC2FX image to the helper that built it.
//
// The cache folder belongs to the desktop user, so `installer.json` and
// `archive.tar.gz` can be rewritten together while the root helper runs. The
// privileged helper therefore builds the image itself, keeps its own copy of
// what it prepared in a root-owned folder and refuses anything else. Stock
// images the catalog pins are checked against those pins; other stock images
// RØDE lists are recorded like RC2FX images. Without a privileged helper
// there is no boundary to defend and nothing is recorded.

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// anchorDirectory finds the helper's root-owned folder for the records. The
// package's tests replace it: run as root, as CI's containers do, they
// would otherwise read, and create, the real /var/lib/rc2fx/prepared.
var anchorDirectory = helperDirectory

// anchorFile is where the record for this cache folder lives; empty when
// unprivileged.
func (e *Engine) anchorFile() (string, error) {
	var dir string
	if e.anchorDir != nil {
		dir = *e.anchorDir
	} else {
		found, err := anchorDirectory("prepared")
		if err != nil {
			return "", err
		}
		dir = found
	}
	if dir == "" {
		return "", nil
	}
	return filepath.Join(dir, sum([]byte(e.Work))), nil
}

// anchorPrepared records prepared, whose hashes this process computed while
// building it.
func (e *Engine) anchorPrepared(prepared *Prepared) error {
	path, err := e.anchorFile()
	if err != nil || path == "" {
		return err
	}
	// Not atomicFile: that gives the file to the desktop user.
	part := strings.TrimSuffix(path, filepath.Ext(path)) + ".part"
	data, err := sjson.Marshal(prepared)
	if err != nil {
		return err
	}
	if err := os.WriteFile(part, data, 0o666); err != nil {
		return err
	}
	return os.Rename(part, path)
}

// checkAnchor refuses a prepared image that this helper did not record.
func (e *Engine) checkAnchor(prepared *Prepared) error {
	path, err := e.anchorFile()
	if err != nil || path == "" {
		return err
	}
	var recorded *Prepared
	data, err := os.ReadFile(path)
	switch {
	case err == nil:
		var p Prepared
		if sjson.Unmarshal(data, &p) == nil {
			recorded = &p
		}
	case !errors.Is(err, fs.ErrNotExist):
		return err
	}
	if recorded == nil || !recorded.Equal(prepared) {
		return Error("this firmware was not prepared by the installer helper; prepare it again")
	}
	return nil
}
