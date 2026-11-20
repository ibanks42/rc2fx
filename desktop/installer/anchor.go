package installer

// Anchors prepared firmware images to the privileged helper that built them.
//
// Because the cache directory is owned by the desktop user, installer.json and
// archive.tar.gz could be tampered with together while the root helper runs.
// The helper therefore records its own hashes in a root-owned directory and
// refuses any image that doesn't match. Pinned stock images are verified
// against the catalog; other RØDE stock images are anchored like RC2FX images.
// Unprivileged runs skip anchoring since there is no privilege boundary.

import (
	"errors"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// Overridden in tests so root CI containers don't touch /var/lib/rc2fx/prepared.
var anchorDirectory = helperDirectory

// anchorFile returns the root-owned anchor path for this cache dir, or "" when unprivileged.
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
	return filepath.Join(dir, archive.Sum([]byte(e.Work))), nil
}

func (e *Engine) anchorPrepared(prepared *Prepared) error {
	path, err := e.anchorFile()
	if err != nil || path == "" {
		return err
	}
	// Don't use atomicFile here; it chowns to the desktop user.
	part := strings.TrimSuffix(path, filepath.Ext(path)) + ".part"
	data, err := sjson.Marshal(prepared)
	if err != nil {
		return err
	}
	if err := os.WriteFile(part, data, 0o600); err != nil {
		return err
	}
	return os.Rename(part, path)
}

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
