package state

import "testing"

// navigationFacts is every question the views ask of a navigation.
type navigationFacts struct {
	home, homeInFront, channelInFront, baseInFront, settings, diagnostics bool
}

func factsOf(n Navigation) navigationFacts {
	return navigationFacts{n.Home(), n.HomeInFront(), n.ChannelInFront(), n.BasePageInFront(),
		n.Settings(), n.Diagnostics()}
}

func TestTheAppStartsOnHome(t *testing.T) {
	var n Navigation
	want := navigationFacts{home: true, homeInFront: true, baseInFront: true}
	if got := factsOf(n); got != want {
		t.Fatalf("start %+v, want %+v", got, want)
	}
}

func TestOnePageIsInFront(t *testing.T) {
	for _, base := range []basePage{pageHome, pageChannel} {
		for _, over := range []overlay{overlayNone, overlaySettings, overlayDiagnostics} {
			f := factsOf(Navigation{base, over})
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
	n := Navigation{base: pageChannel}
	if !n.ShowSettings() {
		t.Fatal("opening Settings was not reported")
	}
	n.SetDiagnostics(true)
	if !n.Diagnostics() || !n.Settings() {
		t.Fatalf("Diagnostics not shown: %+v", factsOf(n))
	}
	// Settings again, from Diagnostics: its main page, not a new opening.
	if n.ShowSettings() {
		t.Fatal("Settings reported as newly opened")
	}
	if n.Diagnostics() {
		t.Fatal("Diagnostics stayed in front")
	}
	n.CloseOverlay()
	if !n.ChannelInFront() {
		t.Fatalf("closing did not show the channel again: %+v", factsOf(n))
	}
}

func TestDiagnosticsOnlyFromSettings(t *testing.T) {
	var n Navigation
	n.SetDiagnostics(true)
	if n.Diagnostics() || !n.HomeInFront() {
		t.Fatalf("Diagnostics opened without Settings: %+v", factsOf(n))
	}
}

func TestTheBasePageStaysUnderOverlays(t *testing.T) {
	var n Navigation
	n.ShowChannel()
	n.ShowSettings()
	if n.Home() || n.ChannelInFront() {
		t.Fatalf("channel under Settings: %+v", factsOf(n))
	}
	// A new connection puts Home behind Settings without closing it.
	n.HomeBehind()
	if !n.Home() || !n.Settings() || n.HomeInFront() {
		t.Fatalf("Home behind Settings: %+v", factsOf(n))
	}
	n.ShowChannel()
	if !n.ChannelInFront() {
		t.Fatalf("opening a channel: %+v", factsOf(n))
	}
	n.ShowSettings()
	n.ShowHome()
	if !n.HomeInFront() {
		t.Fatalf("the overview from Settings: %+v", factsOf(n))
	}
}
