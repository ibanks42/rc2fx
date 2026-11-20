package installflow

import (
	"reflect"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AccessKind tracks privilege elevation for flashing. On Linux without root,
// switching to USB update mode and flashing run inside a pkexec helper;
// everything else runs in-process.
type AccessKind int

const (
	AccessNotNeeded AccessKind = iota
	AccessNeeded
	AccessAsking // OS password prompt is open
	AccessGranted
	AccessRefused // last elevation attempt failed or was dismissed (see Why)
)

// Access pairs AccessKind with an optional failure reason for AccessRefused.
type Access struct {
	Kind AccessKind
	Why  string
}

func (a Access) AllowsFlashing() bool {
	return a.Kind == AccessNotNeeded || a.Kind == AccessGranted
}

// Privileged reports whether request must run in the elevated helper. Prepare
// is included because the helper will only flash images it prepared itself.
func Privileged(request installer.Request) bool {
	switch request.Kind {
	case installer.RequestPrepare, installer.RequestEnterUSBMode, installer.RequestFlash,
		installer.RequestRepairInspect, installer.RequestRepair, installer.RequestStorageFix:
		return true
	}
	return false
}

// IsSelection reports whether request selects the mixer software to install.
func IsSelection(request installer.Request) bool {
	switch request.Kind {
	case installer.RequestSelectMixer, installer.RequestSelectBundledMixer:
		return true
	}
	return false
}

// ChangesCatalog reports whether request can update the catalog that RC2FX
// readiness is evaluated against.
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

func sameRequest(a, b *installer.Request) bool { return reflect.DeepEqual(a, b) }

// Selection tracks which mixer software the local engine and the privileged
// helper have selected. The helper verifies payloads on its own, so it
// receives the same selection request (and needs it re-sent after a restart).
type Selection struct {
	Wanted *installer.Request
	Helper *installer.Request
}

// EngineAnswered updates the wanted selection after the local engine handles
// a selection request. A failed selection clears it so the helper doesn't keep
// stale state.
func (s *Selection) EngineAnswered(request installer.Request, selected bool) {
	if selected {
		s.Wanted = &request
	} else {
		s.Wanted = nil
	}
}

// Clear drops the wanted selection (stock firmware needs no mixer software).
func (s *Selection) Clear() { s.Wanted = nil }

// Pending returns the selection request the helper still needs, or nil if it
// is already in sync.
func (s *Selection) Pending() *installer.Request {
	if s.Wanted == nil || sameRequest(s.Wanted, s.Helper) {
		return nil
	}
	return s.Wanted
}

func (s *Selection) HelperAnswered() { s.Helper = s.Wanted }

// HelperCleared forgets the helper's selection after a restart or before
// sending a new selection request.
func (s *Selection) HelperCleared() { s.Helper = nil }

// RecentlyFlashed reports whether a flash was sent recently and the mixer may
// still be updating (used by the app's restart blocker).
func (m *Model) RecentlyFlashed() bool { return m.recentlyFlashed() }

func ProbeRequest() installer.Request { return installer.Request{Kind: installer.RequestProbe} }

func AllowDuoRequest(allow bool) installer.Request {
	return installer.Request{Kind: installer.RequestAllowExperimentalDuo, Allow: allow}
}

func AllowDuoOf(request installer.Request) (allow, ok bool) {
	if request.Kind != installer.RequestAllowExperimentalDuo {
		return false, false
	}
	return request.Allow, true
}

func IsStockPrepare(request installer.Request) bool {
	return request.Kind == installer.RequestPrepare && request.Stock
}

func MixerReleaseOffers(reply installer.Reply) ([]installer.MixerOffer, bool) {
	if reply.Kind != installer.ReplyMixerReleases {
		return nil, false
	}
	return reply.Offers, true
}
