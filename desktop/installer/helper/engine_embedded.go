//go:build embedded

package helper

import "github.com/ibanks42/rc2fx/desktop/installer"

func engineFor(root, work string) *installer.Engine {
	switch root {
	case Download:
		return installer.DownloadOnly(work)
	case Embedded:
		return installer.Embedded(work)
	}
	return installer.New(root, work)
}

// StartEmbedded launches the privileged helper via pkexec using the payload
// embedded in this executable.
func StartEmbedded(work string) (*Helper, error) {
	return spawn(Embedded, work)
}
