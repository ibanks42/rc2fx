package installflow

import (
	"fmt"
	"iter"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
)

func (m *Model) Choose(index int) {
	if m.Run != nil || m.Busy() {
		return
	}
	if index >= 0 && index < len(m.Releases) && m.Releases[index].Supported {
		r := m.Releases[index]
		m.Choice = &Choice{Channel: r.Channel, Version: r.Version}
	}
	m.settleMixer()
}

func (m *Model) ChooseStock(index int) {
	if m.Run != nil || m.Busy() {
		return
	}
	if index >= 0 && index < len(m.StockReleases) {
		r := m.StockReleases[index]
		m.StockChoice = &Choice{Channel: r.Channel, Version: r.Version}
	}
}

func (m *Model) Chosen() *Choice {
	if m.Stock {
		return m.StockChoice
	}
	return m.Choice
}

func (m *Model) checkStock() *installer.Request {
	if m.StockCheck.Kind != StockCheckUnchecked {
		return nil
	}
	m.StockCheck = StockCheck{Kind: StockCheckChecking}
	return m.quiet(installer.Request{Kind: installer.RequestStockReleases})
}

func (m *Model) RecheckStock() *installer.Request {
	if m.StockCheck.Kind != StockCheckFailed || m.Busy() {
		return nil
	}
	m.StockCheck = StockCheck{Kind: StockCheckUnchecked}
	return m.checkStock()
}

func (m *Model) forgetStock() {
	m.StockReleases = nil
	m.StockChoice = nil
	m.StockCheck = StockCheck{Kind: StockCheckUnchecked}
}

func (m *Model) firmwareProfile(choice *Choice) (string, bool) {
	if choice == nil {
		return "", false
	}
	for _, r := range m.Releases {
		if r.Channel == choice.Channel && r.Version == choice.Version {
			return r.Profile, r.Profile != ""
		}
	}
	return "", false
}

// MixerOptions returns compatible signed mixer offers (newest first) followed
// by the bundled copy, if any.
func (m *Model) MixerOptions() []MixerSource {
	var options []MixerSource
	if profile, ok := m.firmwareProfile(m.Choice); m.Offers.Kind == OffersReady && ok {
		for _, offer := range m.Offers.List {
			if model.OfferFits(offer, profile) {
				options = append(options, Signed(offer))
			}
		}
	}
	if m.Bundled != nil {
		options = append(options, Bundled(*m.Bundled))
	}
	return options
}

func (m *Model) MixerMenu() []MixerMenuItem {
	var menu []MixerMenuItem
	for _, source := range m.MixerOptions() {
		name := source.Version()
		if source.Kind == MixerSourceBundled {
			name = fmt.Sprintf("%s · included with this app", source.Version())
		}
		if m.MixerChange(source) == MixerChangeDowngrade {
			name += " · older than installed"
		}
		chosen := m.Mixer != nil && m.Mixer.Equal(source)
		menu = append(menu, MixerMenuItem{Source: source, Name: name, Chosen: chosen})
	}
	return menu
}

func (m *Model) MixerChange(source MixerSource) MixerChange {
	return change(m.Installed, source.Version())
}

// settleMixer preserves an explicit user selection if still valid; otherwise
// picks the newest non-downgrade signed offer (never auto-selecting a downgrade
// or the bundled fallback).
func (m *Model) settleMixer() {
	if m.Run != nil || m.Offers.Kind == OffersUnchecked || m.Offers.Kind == OffersChecking {
		return
	}
	options := m.MixerOptions()
	if m.mixerChosen && m.Mixer != nil && containsSource(options, *m.Mixer) {
		return
	}
	m.mixerChosen = false
	m.Mixer = nil
	for _, source := range options {
		if source.Kind == MixerSourceSigned && m.MixerChange(source) != MixerChangeDowngrade {
			m.Mixer = &source
			break
		}
	}
}

// InPlaceOffer returns the signed offer if the connected mixer supports
// in-place updates and the offer is an upgrade for its current firmware profile.
func (m *Model) InPlaceOffer(support InPlaceSupport) *installer.MixerOffer {
	if support.Profile == nil {
		return nil
	}
	if !support.Supported || m.Stock || m.Run != nil || !m.Answering() {
		return nil
	}
	if m.Mixer == nil || m.Mixer.Kind != MixerSourceSigned {
		return nil
	}
	offer := &m.Mixer.Offer
	if m.MixerChange(Signed(*offer)) == MixerChangeUpgrade &&
		slices.Contains(offer.Metadata.FirmwareProfiles, *support.Profile) {
		return offer
	}
	return nil
}

func (m *Model) StartInPlace(offer installer.MixerOffer, profile string) *installer.Request {
	if m.Busy() || m.inPlace.Active() || m.Run != nil {
		return nil
	}
	m.inPlace = InPlace{Kind: InPlacePreparing, Version: offer.Metadata.Version}
	var installed *string
	if m.Installed != nil {
		v := *m.Installed
		installed = &v
	}
	return m.send(installer.Request{
		Kind:            installer.RequestInPlaceBundle,
		Offer:           &offer,
		FirmwareProfile: profile,
		Installed:       installed,
	})
}

// TakeInPlaceBundle pops the verified bundle once for upload and transitions
// the in-place state to InPlaceUploading.
func (m *Model) TakeInPlaceBundle() (installer.InPlaceBundle, bool) {
	if m.inPlace.Kind != InPlaceReady {
		return installer.InPlaceBundle{}, false
	}
	bundle := m.inPlace.Bundle
	m.inPlaceBefore = inPlaceBaseline{}
	m.inPlace = InPlace{
		Kind:    InPlaceUploading,
		Version: bundle.Version,
		Sent:    0,
		Total:   bundle.Size,
	}
	return bundle, true
}

func (m *Model) ChooseMixer(source MixerSource) {
	if m.Run == nil && !m.Busy() && containsSource(m.MixerOptions(), source) {
		m.Mixer = &source
		m.mixerChosen = true
	}
}

func (m *Model) SetInstalled(mixer, firmware *string) {
	if !opt.Equal(m.Installed, mixer) || !opt.Equal(m.InstalledFirmware, firmware) {
		m.Installed = mixer
		m.InstalledFirmware = firmware
		m.settleMixer()
	}
}

func (m *Model) checkMixers() *installer.Request {
	if m.Offers.Kind != OffersUnchecked {
		return nil
	}
	m.Offers = Offers{Kind: OffersChecking}
	return m.quiet(installer.Request{Kind: installer.RequestMixerReleases})
}

func (m *Model) RecheckMixers() *installer.Request {
	if m.Offers.Kind != OffersFailed {
		return nil
	}
	m.Offers = Offers{Kind: OffersUnchecked}
	return m.checkMixers()
}

func (m *Model) FirmwareNote() (string, bool) {
	if len(m.NeedsApp) == 0 {
		return "", false
	}
	release := m.NeedsApp[0]
	return fmt.Sprintf("RØDE %s needs RC2FX %s or newer. Update the app first.",
		release.Version, release.MinimumAppVersion), true
}

func (m *Model) MixerNote() (string, bool) {
	fallback := ""
	if m.Bundled != nil {
		fallback = fmt.Sprintf(" You can choose %s, included with this app, instead.", *m.Bundled)
	}
	switch m.Offers.Kind {
	case OffersFailed:
		if m.Offers.Error == installer.Canceled {
			return "The check for mixer software was canceled." + fallback, true
		}
		return fmt.Sprintf("Couldn't check for mixer software. %s%s", m.Offers.Error, fallback), true
	case OffersReady:
		offers := m.Offers.List
		options := m.MixerOptions()
		if len(options) == 0 {
			version := ""
			if m.Choice != nil {
				version = m.Choice.Version
			}
			if need, ok := model.NeedsNewerApp(offers, nil); ok {
				return fmt.Sprintf("Mixer software %s needs RC2FX %s or newer. Update the app first.",
					need.Version, need.MinimumApp), true
			}
			return fmt.Sprintf("No signed mixer software works with RØDE %s and this app.", version), true
		}
		if m.Mixer == nil {
			if m.Installed != nil {
				return fmt.Sprintf("Nothing newer than this mixer's %s is available. Choosing a version below replaces it with an older one.",
					*m.Installed), true
			}
			return "Choose the mixer software to install.", true
		}
		var newest *string
		for _, source := range options {
			version := source.Version()
			if newest == nil {
				newest = &version
				continue
			}
			if c, ok := core.CompareVersions(*newest, version); !ok || c <= 0 {
				newest = &version
			}
		}
		if need, ok := model.NeedsNewerApp(offers, newest); ok {
			return fmt.Sprintf("Mixer software %s needs RC2FX %s or newer. Update the app first to get it.",
				need.Version, need.MinimumApp), true
		}
		return "", false
	default:
		return "", false
	}
}

// WarningsFor returns downgrade or unknown-version warnings for the current selection.
func (m *Model) WarningsFor(stock bool) []string {
	var warnings []string
	if m.Mixer != nil && !stock {
		version := m.Mixer.Version()
		switch m.MixerChange(*m.Mixer) {
		case MixerChangeDowngrade:
			if m.Installed != nil {
				warnings = append(warnings, fmt.Sprintf(
					"This mixer has RC2FX mixer software %s. This replaces it with the older %s.",
					*m.Installed, version))
			}
		case MixerChangeUnknown:
			// Only warn when an older-than-latest version is selected.
			if m.newestMixer(*m.Mixer) {
				break
			}
			warnings = append(warnings, fmt.Sprintf(
				"The app can't tell which RC2FX version this mixer has, if any. If it has one, %s could be older than what it replaces.",
				version))
		}
	}
	choice := m.Choice
	if stock {
		choice = m.StockChoice
	}
	if choice != nil && m.InstalledFirmware != nil &&
		change(m.InstalledFirmware, choice.Version) == MixerChangeDowngrade {
		warnings = append(warnings, fmt.Sprintf(
			"This mixer has RØDE firmware %s. This replaces it with the older %s.",
			*m.InstalledFirmware, choice.Version))
	}
	return warnings
}

func (m *Model) newestMixer(source MixerSource) bool {
	for _, other := range m.MixerOptions() {
		if c, ok := core.CompareVersions(source.Version(), other.Version()); !ok || c < 0 {
			return false
		}
	}
	return true
}

// SetStock switches between RC2FX and stock firmware modes. Stops a failed run
// first so callers like the Home update notice reset cleanly.
func (m *Model) SetStock(stock bool) {
	if m.Running() || m.Busy() {
		return
	}
	if m.Failed() {
		m.Stop()
		if m.Run != nil {
			return
		}
	}
	m.Stock = stock
	m.finished = nil
	if !m.supportedChoice(m.Choice) {
		m.Choice = defaultChoice(m.Releases)
	}
	m.settleMixer()
}

func (m *Model) ReleasesFor() iter.Seq2[int, installer.Release] {
	return func(yield func(int, installer.Release) bool) {
		for i, r := range m.Releases {
			if r.Supported && !yield(i, r) {
				return
			}
		}
	}
}

func (m *Model) ChoiceFor() *Choice {
	if m.supportedChoice(m.Choice) {
		choice := *m.Choice
		return &choice
	}
	return defaultChoice(m.Releases)
}

func (m *Model) Select(stock bool, index int) {
	m.SetStock(stock)
	m.Choose(index)
}
