package app

import (
	"fmt"
	"os"

	"gpuikit.dev/gk"
)

// openDevPage navigates once to the page requested via RC2FX_DEV_PAGE.
// Pages that depend on mixer state wait until the first connection succeeds.
func (a *App) openDevPage(cx *gk.Cx) {
	if a.devPage == nil {
		return
	}
	if a.devPage.NeedsMixer() && !a.session.Connected {
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
		a.nav.SetDiagnostics(true)
	case DevPageInstaller:
		a.openTask(cx, taskInstall)
	case DevPageRepair:
		a.openRepair(cx)
	case DevPageFlow:
		a.openFlowFixture(cx, page.Name)
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
