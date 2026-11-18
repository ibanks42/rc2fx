package installflow

// The installer page's own decisions (the Rust app's installer/mod.rs before):
// administrator access, which requests the privileged helper runs, and which
// mixer software the engine and the helper each hold. Nothing here draws, so
// it tests with plain go test; package app runs the page with it.

import (
	"reflect"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AccessKind is administrator access for the steps that need it: on Linux
// without root, switching the mixer to USB update mode and flashing it run
// in a helper started through pkexec. The page asks for it when the user
// presses the button that starts (or tries again); everything else runs in
// this process.
type AccessKind int

const (
	// AccessNotNeeded: this system lets the app do everything itself.
	AccessNotNeeded AccessKind = iota
	AccessNeeded
	// AccessAsking: the system's password prompt is open.
	AccessAsking
	AccessGranted
	// AccessRefused: the last attempt failed or was dismissed; Why says why.
	AccessRefused
)

// Access is administrator access (mod.rs Access). Why is set for
// AccessRefused only.
type Access struct {
	Kind AccessKind
	Why  string
}

// AllowsFlashing reports access that lets this computer flash the mixer.
func (a Access) AllowsFlashing() bool {
	return a.Kind == AccessNotNeeded || a.Kind == AccessGranted
}

// Privileged reports requests that need administrator access where Access
// applies. The helper flashes only what it prepared, so it does the
// preparing too.
func Privileged(request installer.Request) bool {
	switch request.Kind {
	case installer.RequestPrepare, installer.RequestEnterUSBMode, installer.RequestFlash,
		installer.RequestRepairInspect, installer.RequestRepair, installer.RequestStorageFix:
		return true
	}
	return false
}

// IsSelection reports a request that chooses the mixer software to install
// (mod.rs selection).
func IsSelection(request installer.Request) bool {
	switch request.Kind {
	case installer.RequestSelectMixer, installer.RequestSelectBundledMixer:
		return true
	}
	return false
}

// ChangesCatalog reports a request that can change the catalog RC2FX's
// readiness is judged against, which the mixer's own state says nothing
// about.
func ChangesCatalog(request installer.Request) bool {
	if IsSelection(request) {
		return true
	}
	switch request.Kind {
	case installer.RequestReleases, installer.RequestMixerReleases:
		return true
	}
	return false
}

// sameRequest is Option<Request>'s derived PartialEq: nil is None. Requests
// hold pointers to offers and slices, so it compares what they point to.
func sameRequest(a, b *installer.Request) bool { return reflect.DeepEqual(a, b) }

// Selection is which mixer software the engine and the helper each hold.
// The helper verifies the payload itself, so it gets the same request rather
// than a path or anything else from this process, and a restarted helper
// holds nothing until it is sent again. nil is None.
type Selection struct {
	Wanted *installer.Request
	Helper *installer.Request
}

// EngineAnswered: the engine answered request. A failed selection leaves it
// with nothing selected, which the helper must not outlive.
func (s *Selection) EngineAnswered(request installer.Request, selected bool) {
	if selected {
		s.Wanted = &request
	} else {
		s.Wanted = nil
	}
}

// Clear: stock firmware needs no mixer software, so a helper that lost its
// selection must not have to download one.
func (s *Selection) Clear() { s.Wanted = nil }

// Pending is what the helper must be sent before it acts; nil for nothing.
func (s *Selection) Pending() *installer.Request {
	if s.Wanted == nil || sameRequest(s.Wanted, s.Helper) {
		return nil
	}
	return s.Wanted
}

// HelperAnswered: the helper now holds what the engine selected.
func (s *Selection) HelperAnswered() { s.Helper = s.Wanted }

// HelperCleared: the helper holds nothing: it restarted, or it was sent a
// selection, which clears what it had until that selection succeeds.
func (s *Selection) HelperCleared() { s.Helper = nil }

// RecentlyFlashed reports that the last USB command was a flash in the last
// few minutes, so the mixer may still be updating (Model::recently_flashed,
// which the app's restart blocker reads).
func (m *Model) RecentlyFlashed() bool { return m.recentlyFlashed() }

// The requests and replies the page's runner makes or reads itself, so the
// Request and Reply forms are spelled out only in this package.

// ProbeRequest is Request::Probe, which a newly started helper answers to
// show it is up.
func ProbeRequest() installer.Request { return installer.Request{Kind: installer.RequestProbe} }

// AllowDuoRequest is Request::AllowExperimentalDuo(allow).
func AllowDuoRequest(allow bool) installer.Request {
	return installer.Request{Kind: installer.RequestAllowExperimentalDuo, Allow: allow}
}

// AllowDuoOf is the setting an AllowExperimentalDuo request carries; ok
// false for any other request.
func AllowDuoOf(request installer.Request) (allow, ok bool) {
	if request.Kind != installer.RequestAllowExperimentalDuo {
		return false, false
	}
	return request.Allow, true
}

// IsStockPrepare reports Request::Prepare { stock: true, .. }.
func IsStockPrepare(request installer.Request) bool {
	return request.Kind == installer.RequestPrepare && request.Stock
}

// MixerReleaseOffers is what a Reply::MixerReleases found; ok false for any
// other reply.
func MixerReleaseOffers(reply installer.Reply) ([]installer.MixerOffer, bool) {
	if reply.Kind != installer.ReplyMixerReleases {
		return nil, false
	}
	return reply.Offers, true
}
