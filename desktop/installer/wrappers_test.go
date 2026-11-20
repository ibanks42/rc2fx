package installer

import (
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

// Test-only convenience wrappers around production helpers.

func patchArchive(src, dst string, extra []archive.Entry) (string, string, error) {
	p, err := patchArchiveForModel(firmware.ModelProIi, src, dst, extra, nil)
	if err != nil {
		return "", "", err
	}
	return p.Mixer, p.Launcher, nil
}

func verifyArchive(dir string, p *Prepared) error {
	return archiveMatches(dir, p, func(uint64, uint64) {})
}

func ServiceFromState(root string, state []byte) Service {
	return folderSource(root).service(state)
}

func WithPlatform(root, work string, p Platform) *Engine {
	return withSourcePlatform(folderSource(root), work, p)
}
