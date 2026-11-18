package model

import (
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func protocol(delta int) *uint32 {
	p := uint32(int(core.MixerProtocolVersion) + delta)
	return &p
}

func inputs(rt, fw *string) Inputs {
	return Inputs{
		App:                     "0.3.0",
		InstalledRuntime:        rt,
		AvailableRuntime:        new("0.3.0"),
		SignedKnown:             true,
		NeedsApp:                nil,
		InstalledFirmware:       fw,
		NewestSupportedFirmware: new("1.8.0"),
		RodeFirmware:            nil,
		DeviceProtocol:          protocol(0),
	}
}

func current() AppStanding {
	return AppStanding{Kind: AppStandingCurrent, Latest: "0.3.0"}
}

func appUpdate() AppStanding {
	return AppStanding{Kind: AppStandingUpdate, Version: "0.4.0"}
}

func line(t *testing.T, o Overview, part Part) Line {
	t.Helper()
	for _, l := range o.Lines {
		if l.Part == part {
			return l
		}
	}
	t.Fatalf("no line for part %v", part)
	return Line{}
}

func anyDetail(o Overview, f func(string) bool) bool {
	return slices.ContainsFunc(o.Details, f)
}

func contains(sub string) func(string) bool {
	return func(s string) bool { return strings.Contains(s, sub) }
}

func bundled(version string) installer.MixerUpdateState {
	return installer.MixerUpdateState{Kind: installer.MixerBundled, Version: version}
}

func TestNewerRodeFirmwareRC2FXCantInstallIsNotCalledUpToDate(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Lead.Title != "Everything is up to date" {
		t.Fatalf("lead %q", o.Lead.Title)
	}

	i.RodeFirmware = new("1.9.0")
	o = OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Lead.Title != "RC2FX is up to date" {
		t.Fatalf("lead %q", o.Lead.Title)
	}
	if o.Lead.Detail == nil || !strings.Contains(*o.Lead.Detail, "1.9.0") {
		t.Fatalf("detail %v", o.Lead.Detail)
	}
	firmware := o.Lines[2]
	if (statusTone{firmware.Status, firmware.Tone}) != rodeNewer {
		t.Fatalf("firmware %q %v", firmware.Status, firmware.Tone)
	}
	if firmware.Available != "1.9.0" {
		t.Fatalf("available %q", firmware.Available)
	}
	if o.OfferInstaller {
		t.Fatal("nothing this app can install")
	}

	// RØDE's newest is what RC2FX supports, or what the mixer has.
	for _, rode := range []string{"1.8.0", "1.7.0"} {
		i.RodeFirmware = new(rode)
		o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
		if o.Lead.Title != "Everything is up to date" {
			t.Fatalf("%s: lead %q", rode, o.Lead.Title)
		}
	}

	// An update RC2FX can install still leads; RØDE's news is a note.
	i.InstalledFirmware = new("1.7.0")
	i.RodeFirmware = new("1.9.0")
	o = OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Lines[2].Tone != ToneUpdate {
		t.Fatalf("tone %v", o.Lines[2].Tone)
	}
	if !anyDetail(o, contains("1.9.0")) {
		t.Fatalf("details %q", o.Details)
	}
}

func TestProtocolMismatchBlocksUpdatesAndSaysUpdateTheApp(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.DeviceProtocol = protocol(1)
	s := Summarize(&i)
	if s.UpdateAvailable() {
		t.Fatal("update available")
	}
	n, ok := s.Notice()
	if !ok || n.OfferInstaller || !strings.Contains(n.Text, "Update the app") {
		t.Fatalf("notice %+v %v", n, ok)
	}
}

func TestOlderProtocolOffersCompatibleMixerSoftware(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.DeviceProtocol = protocol(-1)
	s := Summarize(&i)
	if s.Compatibility != CompatibilityMixerOlder {
		t.Fatalf("compatibility %v", s.Compatibility)
	}
	n, ok := s.Notice()
	if !ok || !n.OfferInstaller || !strings.Contains(n.Text, "older than this app supports") {
		t.Fatalf("notice %+v %v", n, ok)
	}
	// Never if the combined image would take the firmware backwards.
	i.InstalledFirmware = new("1.9.0")
	if n, ok := Summarize(&i).Notice(); !ok || n.OfferInstaller {
		t.Fatalf("notice %+v %v", n, ok)
	}
	i.InstalledFirmware = nil
	if n, ok := Summarize(&i).Notice(); !ok || n.OfferInstaller {
		t.Fatalf("notice %+v %v", n, ok)
	}
}

func TestUnknownProtocolNeverOffersAnUpdate(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.DeviceProtocol = nil
	s := Summarize(&i)
	if s.Compatibility != CompatibilityUnknown {
		t.Fatalf("compatibility %v", s.Compatibility)
	}
	if s.UpdateAvailable() {
		t.Fatal("update available")
	}
	if n, ok := s.Notice(); ok {
		t.Fatalf("notice %+v", n)
	}
}

func TestComparesNumerically(t *testing.T) {
	for _, c := range []struct {
		a, b string
		want int
		ok   bool
	}{
		{"1.7.6", "1.10.0", -1, true},
		{"1.8", "1.8.0", 0, true},
		{"2.0.0", "1.9.9", 1, true},
		{"1.8.0-rc1", "1.8.0", 0, false},
		{"", "1.0", 0, false},
	} {
		got, ok := Compare(c.a, c.b)
		if ok != c.ok || (ok && sign(got) != c.want) {
			t.Fatalf("Compare(%q, %q) = %d, %v", c.a, c.b, got, ok)
		}
	}
}

func sign(c int) int {
	switch {
	case c < 0:
		return -1
	case c > 0:
		return 1
	}
	return 0
}

func TestUnknownDeviceVersionsNeverPrompt(t *testing.T) {
	i := inputs(nil, nil)
	s := Summarize(&i)
	if s.Runtime != StandingUnknown || s.Firmware != StandingUnknown {
		t.Fatalf("summary %+v", s)
	}
	if s.UpdateAvailable() {
		t.Fatal("update available")
	}
	if n, ok := s.Notice(); ok {
		t.Fatalf("notice %+v", n)
	}
}

func TestUnparseableIsUnknown(t *testing.T) {
	if got := StandingOf(new("dev-build"), new("0.3.0")); got != StandingUnknown {
		t.Fatalf("standing %v", got)
	}
}

func TestNewerDeviceIsNotADowngradeOffer(t *testing.T) {
	i := inputs(new("0.4.0"), new("1.9.0"))
	s := Summarize(&i)
	if s.Runtime != StandingNewer || s.Firmware != StandingNewer {
		t.Fatalf("summary %+v", s)
	}
	if s.UpdateAvailable() {
		t.Fatal("update available")
	}
	n, ok := s.Notice()
	if !ok || n.OfferInstaller || !strings.Contains(n.Text, "Update the app first") {
		t.Fatalf("notice %+v %v", n, ok)
	}
}

func TestMixedVersionsNeverOfferTheCombinedFlash(t *testing.T) {
	// Older mixer software but newer RØDE firmware, and the reverse.
	for _, c := range [][2]string{{"0.2.0", "1.9.0"}, {"0.4.0", "1.7.6"}} {
		i := inputs(new(c[0]), new(c[1]))
		s := Summarize(&i)
		if s.UpdateAvailable() {
			t.Fatalf("%s %s", c[0], c[1])
		}
		if n, ok := s.Notice(); !ok || n.OfferInstaller {
			t.Fatalf("%s %s: %+v %v", c[0], c[1], n, ok)
		}
	}
}

func TestOneUnknownPartNeverOffersTheCombinedFlash(t *testing.T) {
	for _, c := range [][2]*string{{new("0.2.0"), nil}, {nil, new("1.7.6")}} {
		i := inputs(c[0], c[1])
		s := Summarize(&i)
		if s.UpdateAvailable() {
			t.Fatal("update available")
		}
		if n, ok := s.Notice(); ok {
			t.Fatalf("notice %+v", n)
		}
	}
}

func TestOlderRuntimeOffersFlashAndRestart(t *testing.T) {
	i := inputs(new("0.2.1"), new("1.8.0"))
	s := Summarize(&i)
	if s.Runtime != StandingUpdateAvailable {
		t.Fatalf("runtime %v", s.Runtime)
	}
	if s.Firmware != StandingCurrent {
		t.Fatalf("firmware %v", s.Firmware)
	}
	n, ok := s.Notice()
	if !ok || !n.OfferInstaller {
		t.Fatalf("notice %+v %v", n, ok)
	}
	if !strings.Contains(n.Text, "RC2FX mixer software") || !strings.Contains(n.Text, "flashes") {
		t.Fatalf("text %q", n.Text)
	}
	if !strings.Contains(n.Text, "restarts") {
		t.Fatalf("text %q", n.Text)
	}
}

func TestOlderFirmwareOffersUpdate(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.7.6"))
	n, ok := Summarize(&i).Notice()
	if !ok || !strings.Contains(n.Text, "RØDE firmware") {
		t.Fatalf("notice %+v %v", n, ok)
	}
}

func TestAnAppUpdateLeadsAndOffersItsChanges(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, appUpdate(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Lead.Title != "RC2FX 0.4.0 is available" {
		t.Fatalf("lead %q", o.Lead.Title)
	}
	if o.Lead.Detail == nil || *o.Lead.Detail != "You have 0.3.0." {
		t.Fatalf("detail %v", o.Lead.Detail)
	}
	if o.Lead.Changes == nil || *o.Lead.Changes != PartApp {
		t.Fatalf("changes %v", o.Lead.Changes)
	}
}

func TestAMixerUpdateLeadsWhenTheAppIsCurrent(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.7.0"))
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if o.Lead.Title != "A mixer update is ready" {
		t.Fatalf("lead %q", o.Lead.Title)
	}
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
}

func TestNothingToDoSaysSoAndOffersNothing(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Lead.Title != "Everything is up to date" {
		t.Fatalf("lead %q", o.Lead.Title)
	}
	if o.Lead.Changes != nil {
		t.Fatalf("changes %v", *o.Lead.Changes)
	}
}

func TestAMixerNewerThanTheAppPointsAtTheAppUpdate(t *testing.T) {
	i := inputs(new("0.4.0"), new("1.8.0"))
	i.AvailableRuntime = new("0.3.0")
	waiting := OverviewOf(&i, appUpdate(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if got := line(t, waiting, PartMixer).Status; got != "Update the app first" {
		t.Fatalf("status %q", got)
	}
	noUpdate := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if got := line(t, noUpdate, PartMixer).Status; got != "Newer" {
		t.Fatalf("status %q", got)
	}
}

func TestAnUnknownLatestVersionIsOnlySaidWhenNothingBetterIs(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerUnknown})
	if !slices.Equal(o.Details, []string{"The latest published mixer software is unknown."}) {
		t.Fatalf("details %q", o.Details)
	}
	i = inputs(new("0.3.0"), new("1.8.0"))
	i.NeedsApp = &AppNeed{Version: "9.0.0", MinimumApp: "99.0.0"}
	o = OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerUnknown})
	if anyDetail(o, contains("unknown")) {
		t.Fatalf("details %q", o.Details)
	}
}

func offer(version, minimumApp string, protocol uint32) installer.MixerOffer {
	return installer.MixerOffer{
		Target: "mixer-" + version + ".payload",
		SHA256: strings.Repeat("00", 32),
		Length: 1,
		Metadata: installer.MixerTarget{
			Version:           version,
			PayloadFormat:     1,
			ProtocolVersion:   protocol,
			MinimumAppVersion: minimumApp,
			FirmwareProfiles:  []string{"rodecaster-pro-ii-1.7.7"},
		},
	}
}

func TestOnlyCompatibleOffersCountAsAvailable(t *testing.T) {
	protocol := uint32(core.MixerProtocolVersion)
	offers := []installer.MixerOffer{
		offer("9.0.0", "99.0.0", protocol),
		offer("8.0.0", "0.0.1", protocol+1),
		offer("2.0.0", "0.0.1", protocol),
	}
	if got, ok := NewestAvailable(offers, nil); !ok || got != "2.0.0" {
		t.Fatalf("newest %q %v", got, ok)
	}
	if got, ok := NewestAvailable(offers, new("3.0.0")); !ok || got != "3.0.0" {
		t.Fatalf("a newer built-in copy is available too: %q %v", got, ok)
	}
	if got, ok := NewestAvailable(nil, nil); ok {
		t.Fatalf("newest %q", got)
	}
}

func TestNewerSoftwareThatNeedsANewerAppIsNamedWithTheVersionItNeeds(t *testing.T) {
	protocol := uint32(core.MixerProtocolVersion)
	offers := []installer.MixerOffer{
		offer("9.0.0", "99.0.0", protocol),
		offer("2.0.0", "0.0.1", protocol),
	}
	needed := AppNeed{Version: "9.0.0", MinimumApp: "99.0.0"}
	if got, ok := NeedsNewerApp(offers, new("2.0.0")); !ok || got != needed {
		t.Fatalf("needs %+v %v", got, ok)
	}
	if got, ok := NeedsNewerApp(offers, nil); !ok || got != needed {
		t.Fatalf("needs %+v %v", got, ok)
	}
	if got, ok := NeedsNewerApp(offers, new("9.0.0")); ok {
		t.Fatalf("needs %+v", got)
	}
	if got, ok := NeedsNewerApp(offers[1:], new("1.0.0")); ok {
		t.Fatalf("needs %+v", got)
	}

	i := inputs(new("0.3.0"), new("1.8.0"))
	i.NeedsApp = &AppNeed{Version: "9.0.0", MinimumApp: "99.0.0"}
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	mixer := line(t, o, PartMixer)
	if mixer.Status != "Needs app update" || mixer.Available != "9.0.0" {
		t.Fatalf("mixer %q %q", mixer.Status, mixer.Available)
	}
	if o.OfferInstaller {
		t.Fatal("nothing here can install it")
	}
	if !anyDetail(o, contains("RC2FX 99.0.0")) {
		t.Fatalf("details %q", o.Details)
	}
}

func TestWhatCanBeInstalledNowStillComesFirst(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.NeedsApp = &AppNeed{Version: "9.0.0", MinimumApp: "99.0.0"}
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if got := line(t, o, PartMixer).Status; got != "Update available" {
		t.Fatalf("status %q", got)
	}
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
	if !anyDetail(o, contains("RC2FX 99.0.0")) {
		t.Fatalf("details %q", o.Details)
	}
}

func TestASignedUpdateNeedsNoPayloadInTheApp(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.AvailableRuntime = new("0.3.0")
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
	if got := line(t, o, PartMixer).Status; got != "Update available" {
		t.Fatalf("status %q", got)
	}
}

func TestWithoutASignedAnswerTheAppIsNotBlamedAndTheInstallerCanCheck(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	i.AvailableRuntime = nil
	i.SignedKnown = false
	state := installer.MixerUpdateState{Kind: installer.MixerAppUpdateRequired, Latest: "0.4.0", AppVersion: "0.4.0"}
	o := OverviewOf(&i, current(), state)
	if got := line(t, o, PartMixer).Status; got != "Unknown" {
		t.Fatalf("status %q", got)
	}
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
	if anyDetail(o, contains("RC2FX 0.4.0")) {
		t.Fatalf("details %q", o.Details)
	}
	if !anyDetail(o, contains("signed mixer software")) {
		t.Fatalf("details %q", o.Details)
	}
	// Newer than anything known: nothing to look for.
	i.InstalledFirmware = new("1.9.0")
	if OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerUnknown}).OfferInstaller {
		t.Fatal("installer offered")
	}
}

func TestAPublishedMixerNeverOffersTheInstaller(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	state := installer.MixerUpdateState{Kind: installer.MixerAppUpdateRequired, Latest: "0.4.0", AppVersion: "0.4.0"}
	o := OverviewOf(&i, current(), state)
	if o.OfferInstaller {
		t.Fatal("installer offered")
	}
	mixer := line(t, o, PartMixer)
	if mixer.Status != "Needs app update" || mixer.Available != "0.4.0" {
		t.Fatalf("mixer %q %q", mixer.Status, mixer.Available)
	}
	if !mixer.ReadChanges {
		t.Fatal("no changes to read")
	}
	if !anyDetail(o, contains("RC2FX 0.4.0")) {
		t.Fatalf("details %q", o.Details)
	}
}

func TestABundledUpdateOffersTheInstallerAndItsChanges(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
	if got := line(t, o, PartMixer).Status; got != "Update available" {
		t.Fatalf("status %q", got)
	}
	if !line(t, o, PartMixer).ReadChanges {
		t.Fatal("no changes to read")
	}
	if o.Flag != "Update available" || o.FlagTone != ToneUpdate {
		t.Fatalf("flag %q %v", o.Flag, o.FlagTone)
	}
	if o.Summary != "App 0.3.0 · Mixer 0.2.0 · Firmware 1.8.0" {
		t.Fatalf("summary %q", o.Summary)
	}
}

func TestABlockedUpdateExplainsItselfAndOffersNothing(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.9.0"))
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if o.OfferInstaller {
		t.Fatal("installer offered")
	}
	if got := line(t, o, PartMixer).Status; got != "Blocked" {
		t.Fatalf("status %q", got)
	}
	if got := line(t, o, PartFirmware).Status; got != "Newer" {
		t.Fatalf("status %q", got)
	}
	if !anyDetail(o, contains("Update the app first")) {
		t.Fatalf("details %q", o.Details)
	}
	if o.FlagTone != ToneWarn {
		t.Fatalf("flag tone %v", o.FlagTone)
	}
}

func TestTooOldMixerSoftwareIsTheMostUrgent(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.8.0"))
	i.DeviceProtocol = protocol(-1)
	o := OverviewOf(&i, current(), bundled("0.3.0"))
	if !o.OfferInstaller {
		t.Fatal("no installer offered")
	}
	if got := line(t, o, PartMixer).Status; got != "Too old" {
		t.Fatalf("status %q", got)
	}
	if o.Flag != "Needs update" || o.FlagTone != ToneBad {
		t.Fatalf("flag %q %v", o.Flag, o.FlagTone)
	}
}

func TestADisconnectedMixerStaysUnknown(t *testing.T) {
	i := inputs(nil, nil)
	i.DeviceProtocol = nil
	i.NewestSupportedFirmware = nil
	o := OverviewOf(&i, AppStanding{Kind: AppStandingUnknown}, installer.MixerUpdateState{Kind: installer.MixerUnknown})
	if o.Flag != "Not connected" {
		t.Fatalf("flag %q", o.Flag)
	}
	if got := line(t, o, PartMixer).Installed; got != "Unknown" {
		t.Fatalf("installed %q", got)
	}
	for _, l := range o.Lines {
		if l.ReadChanges {
			t.Fatalf("line %+v reads changes", l)
		}
	}
	if len(o.Details) != 0 {
		t.Fatalf("details %q", o.Details)
	}
}

func TestAnUnknownPublishedVersionStaysVisible(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, AppStanding{Kind: AppStandingUnknown}, installer.MixerUpdateState{Kind: installer.MixerUnknown})
	if got := line(t, o, PartMixer).Status; got != "Up to date" {
		t.Fatalf("status %q", got)
	}
	if !anyDetail(o, contains("published mixer software is unknown")) {
		t.Fatalf("details %q", o.Details)
	}
}

func TestNewerSoftwareOffersNothing(t *testing.T) {
	i := inputs(new("0.4.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerNewer})
	if got := line(t, o, PartMixer).Status; got != "Newer" {
		t.Fatalf("status %q", got)
	}
	if line(t, o, PartMixer).ReadChanges {
		t.Fatal("changes to read")
	}
	if o.OfferInstaller {
		t.Fatal("installer offered")
	}
}

func TestUpdatesAreCountedAndACheckingAppSaysSo(t *testing.T) {
	i := inputs(new("0.2.0"), new("1.7.0"))
	state := bundled("0.3.0")
	o := OverviewOf(&i, AppStanding{Kind: AppStandingUpdate, Version: "0.4.0"}, state)
	if o.Flag != "3 updates available" {
		t.Fatalf("flag %q", o.Flag)
	}
	for _, l := range o.Lines {
		if !l.ReadChanges {
			t.Fatalf("line %+v has no changes to read", l)
		}
	}
	o = OverviewOf(&i, AppStanding{Kind: AppStandingChecking}, state)
	if got := line(t, o, PartApp).Status; got != "Checking…" {
		t.Fatalf("status %q", got)
	}
}

func TestAllCurrentIsUpToDate(t *testing.T) {
	i := inputs(new("0.3.0"), new("1.8.0"))
	o := OverviewOf(&i, current(), installer.MixerUpdateState{Kind: installer.MixerCurrent})
	if o.Flag != "Up to date" || o.FlagTone != ToneQuiet {
		t.Fatalf("flag %q %v", o.Flag, o.FlagTone)
	}
	for _, l := range o.Lines {
		if l.ReadChanges {
			t.Fatalf("line %+v reads changes", l)
		}
	}
	if len(o.Details) != 0 {
		t.Fatalf("details %q", o.Details)
	}
}
