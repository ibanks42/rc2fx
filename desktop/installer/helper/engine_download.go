//go:build !embedded

package helper

import "github.com/ibanks42/rc2fx/desktop/installer"

// engineFor is the engine a helper serves: a download-only engine for
// Download, otherwise the folder named.
func engineFor(root, work string) *installer.Engine {
	if root == Download {
		return installer.DownloadOnly(work)
	}
	return installer.New(root, work)
}
