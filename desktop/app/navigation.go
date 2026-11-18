package app

// Which page is in front while the app is connected. The main column shows
// a base page, Home (the overview) or the selected channel's editor, and at
// most one overlay above it: Settings or Settings' Diagnostics page. Closing
// the overlay shows the base page again, as it was left. Home stays the base
// page under an overlay because Home's readings and redraws (homeKey) carry
// on while Settings is in front. A guided task (App.installer) takes the
// whole window over all of this while it is open.
//
// Kept as one value so impossible combinations cannot be stored: Home and a
// channel both in front, or Diagnostics without Settings.

// basePage is the page under any overlay.
type basePage uint8

const (
	// pageHome is the overview of every channel.
	pageHome basePage = iota
	// pageChannel is the selected channel's editor (or its unprocessed
	// source panel).
	pageChannel
)

// overlay is the page shown over the base page, if any.
type overlay uint8

const (
	overlayNone overlay = iota
	overlaySettings
	// overlayDiagnostics is Settings' Diagnostics page (telemetry, events,
	// files): Settings, scrolled to another page.
	overlayDiagnostics
)

// navigation is the base page and the overlay in front of it. The zero
// value is Home with nothing over it.
type navigation struct {
	base    basePage
	overlay overlay
}

// home is whether Home is the base page, in front or under an overlay.
func (n navigation) home() bool { return n.base == pageHome }

// homeInFront is whether Home is what the main column shows.
func (n navigation) homeInFront() bool { return n.base == pageHome && n.overlay == overlayNone }

// channelInFront is whether the selected channel's page is what the main
// column shows.
func (n navigation) channelInFront() bool { return n.base == pageChannel && n.overlay == overlayNone }

// basePageInFront is whether nothing is over the base page.
func (n navigation) basePageInFront() bool { return n.overlay == overlayNone }

// settings is whether Settings is in front, on its main page or on
// Diagnostics.
func (n navigation) settings() bool {
	return n.overlay == overlaySettings || n.overlay == overlayDiagnostics
}

// diagnostics is whether Settings is in front on its Diagnostics page.
func (n navigation) diagnostics() bool { return n.overlay == overlayDiagnostics }

// showHome makes Home the base page and closes any overlay.
func (n *navigation) showHome() {
	n.base = pageHome
	n.overlay = overlayNone
}

// showChannel makes the selected channel the base page and closes any
// overlay.
func (n *navigation) showChannel() {
	n.base = pageChannel
	n.overlay = overlayNone
}

// homeBehind makes Home the base page, leaving any overlay in front of it.
func (n *navigation) homeBehind() { n.base = pageHome }

// showSettings brings Settings to the front on its main page, from wherever
// it was (Diagnostics included). It reports whether Settings was not in front
// before, so the caller can load what it shows.
func (n *navigation) showSettings() (opened bool) {
	opened = !n.settings()
	n.overlay = overlaySettings
	return opened
}

// setDiagnostics switches Settings between its main page and Diagnostics.
// Nothing changes while Settings is not in front.
func (n *navigation) setDiagnostics(open bool) {
	if !n.settings() {
		return
	}
	if open {
		n.overlay = overlayDiagnostics
	} else {
		n.overlay = overlaySettings
	}
}

// closeOverlay shows the base page again.
func (n *navigation) closeOverlay() { n.overlay = overlayNone }
