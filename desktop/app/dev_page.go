package app

// dev_page.rs, the navigating half (the parsing is in devpage.go).

import (
	"fmt"
	"os"

	"gpuikit.dev/gk"
)

// openDevPage opens the requested page, once. Pages that need the mixer wait
// for its first state.
func (a *App) openDevPage(cx *gk.Cx) {
	if a.devPage == nil {
		return
	}
	if a.devPage.needsMixer() && !a.session.Connected {
		return
	}
	page := *a.devPage
	a.devPage = nil
	switch page.Kind {
	case DevPageHome:
		a.openOverview(cx)
	case DevPageSettings:
		a.setSettingsOpen(true)
	case DevPageDiagnostics:
		a.setSettingsOpen(true)
		a.diagnosticsOpen = true
	case DevPageInstaller:
		a.toggleInstallerPage(cx)
	case DevPageRepair:
		a.openRepair(cx)
	case DevPageChannel:
		names := make([][2]string, len(a.session.Draft))
		for i, t := range a.session.Draft {
			names[i] = [2]string{t.ID, t.Name}
		}
		if index, ok := findChannel(names, page.Name); ok {
			if page.Effect != "" {
				a.effect = page.Effect
			}
			a.openChannel(cx, index)
		} else {
			fmt.Fprintf(os.Stderr, "RC2FX_DEV_PAGE: no channel named `%s`\n", page.Name)
		}
	}
	a.notify(cx)
}
