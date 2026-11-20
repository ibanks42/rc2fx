package state

// Navigation tracks the active base page (Home or Channel) and at most one
// overlay (Settings or Settings > Diagnostics) while connected. Representing
// both together prevents invalid combinations like Diagnostics without Settings.

type basePage uint8

const (
	pageHome basePage = iota
	pageChannel
)

type overlay uint8

const (
	overlayNone overlay = iota
	overlaySettings
	overlayDiagnostics
)

// Navigation holds the current base page and overlay. The zero value is Home
// with no overlay.
type Navigation struct {
	base    basePage
	overlay overlay
}

// Home reports whether Home is the base page (even when covered by an overlay).
func (n Navigation) Home() bool { return n.base == pageHome }

func (n Navigation) HomeInFront() bool { return n.base == pageHome && n.overlay == overlayNone }

func (n Navigation) ChannelInFront() bool { return n.base == pageChannel && n.overlay == overlayNone }

func (n Navigation) BasePageInFront() bool { return n.overlay == overlayNone }

func (n Navigation) Settings() bool {
	return n.overlay == overlaySettings || n.overlay == overlayDiagnostics
}

func (n Navigation) Diagnostics() bool { return n.overlay == overlayDiagnostics }

func (n *Navigation) ShowHome() {
	n.base = pageHome
	n.overlay = overlayNone
}

func (n *Navigation) ShowChannel() {
	n.base = pageChannel
	n.overlay = overlayNone
}

// HomeBehind switches the base page to Home without closing any active overlay.
func (n *Navigation) HomeBehind() { n.base = pageHome }

// ShowSettings brings Settings to the front and reports whether it was not
// already open.
func (n *Navigation) ShowSettings() (opened bool) {
	opened = !n.Settings()
	n.overlay = overlaySettings
	return opened
}

// SetDiagnostics switches between Settings and Diagnostics when Settings is open.
func (n *Navigation) SetDiagnostics(open bool) {
	if !n.Settings() {
		return
	}
	if open {
		n.overlay = overlayDiagnostics
	} else {
		n.overlay = overlaySettings
	}
}

func (n *Navigation) CloseOverlay() { n.overlay = overlayNone }
