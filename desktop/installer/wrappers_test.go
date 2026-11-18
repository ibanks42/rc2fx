package installer

import "github.com/ibanks42/rc2fx/desktop/firmware"

// Short forms of production functions for the tests: the Pro II's model,
// a folder root, or a check without progress.

func patchArchive(src, dst string, extra []Entry) (string, string, error) {
	p, err := patchArchiveForModel(firmware.ModelProIi, src, dst, extra, nil)
	if err != nil {
		return "", "", err
	}
	return p.Mixer, p.Launcher, nil
}

func verifyArchive(dir string, p *Prepared) error {
	return archiveMatches(dir, p, func(uint64, uint64) {})
}

// ServiceFromState is what RC2FX's state (`GET api/v1/state`, as the app's
// USB connection reads it) says about readiness, judged against the
// installer folder's runtime profile.
func ServiceFromState(root string, state []byte) Service {
	return folderSource(root).service(state)
}

// WithPlatform is New with a given platform.
func WithPlatform(root, work string, p Platform) *Engine {
	return withSourcePlatform(folderSource(root), work, p)
}
