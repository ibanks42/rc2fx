package app

import "testing"

// navigationFacts is every question the views ask of a navigation.
type navigationFacts struct {
	home, homeInFront, channelInFront, baseInFront, settings, diagnostics bool
}

func factsOf(n navigation) navigationFacts {
	return navigationFacts{n.home(), n.homeInFront(), n.channelInFront(), n.basePageInFront(),
		n.settings(), n.diagnostics()}
}

func TestTheAppStartsOnHome(t *testing.T) {
	var n navigation
	want := navigationFacts{home: true, homeInFront: true, baseInFront: true}
	if got := factsOf(n); got != want {
		t.Fatalf("start %+v, want %+v", got, want)
	}
}

func TestOnePageIsInFront(t *testing.T) {
	for _, base := range []basePage{pageHome, pageChannel} {
		for _, over := range []overlay{overlayNone, overlaySettings, overlayDiagnostics} {
			f := factsOf(navigation{base, over})
			front := 0
			for _, shown := range []bool{f.homeInFront, f.channelInFront, f.settings} {
				if shown {
					front++
				}
			}
			if front != 1 {
				t.Errorf("base %d overlay %d: %d pages in front (%+v)", base, over, front, f)
			}
			if f.diagnostics && !f.settings {
				t.Errorf("base %d overlay %d: Diagnostics without Settings", base, over)
			}
			if f.home != (base == pageHome) {
				t.Errorf("base %d overlay %d: home %v", base, over, f.home)
			}
		}
	}
}

func TestSettingsAndDiagnostics(t *testing.T) {
	n := navigation{base: pageChannel}
	if !n.showSettings() {
		t.Fatal("opening Settings was not reported")
	}
	n.setDiagnostics(true)
	if !n.diagnostics() || !n.settings() {
		t.Fatalf("Diagnostics not shown: %+v", factsOf(n))
	}
	// Settings again, from Diagnostics: its main page, not a new opening.
	if n.showSettings() {
		t.Fatal("Settings reported as newly opened")
	}
	if n.diagnostics() {
		t.Fatal("Diagnostics stayed in front")
	}
	n.closeOverlay()
	if !n.channelInFront() {
		t.Fatalf("closing did not show the channel again: %+v", factsOf(n))
	}
}

func TestDiagnosticsOnlyFromSettings(t *testing.T) {
	var n navigation
	n.setDiagnostics(true)
	if n.diagnostics() || !n.homeInFront() {
		t.Fatalf("Diagnostics opened without Settings: %+v", factsOf(n))
	}
}

func TestTheBasePageStaysUnderOverlays(t *testing.T) {
	var n navigation
	n.showChannel()
	n.showSettings()
	if n.home() || n.channelInFront() {
		t.Fatalf("channel under Settings: %+v", factsOf(n))
	}
	// A new connection puts Home behind Settings without closing it.
	n.homeBehind()
	if !n.home() || !n.settings() || n.homeInFront() {
		t.Fatalf("Home behind Settings: %+v", factsOf(n))
	}
	n.showChannel()
	if !n.channelInFront() {
		t.Fatalf("opening a channel: %+v", factsOf(n))
	}
	n.showSettings()
	n.showHome()
	if !n.homeInFront() {
		t.Fatalf("the overview from Settings: %+v", factsOf(n))
	}
}
