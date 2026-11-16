//go:build embedded

package helper

import "github.com/ibanks42/rc2fx/desktop/installer"

// engineFor is the engine a helper serves: the payload embedded in this
// executable when root is Embedded, otherwise the folder named.
func engineFor(root, work string) *installer.Engine {
	switch root {
	case Download:
		return installer.DownloadOnly(work)
	case Embedded:
		return installer.Embedded(work)
	}
	return installer.New(root, work)
}

// StartEmbedded starts the helper for the payload embedded in this
// executable through `pkexec`, which asks for administrator permission once.
func StartEmbedded(work string) (*Helper, error) {
	return spawn(Embedded, work)
}
