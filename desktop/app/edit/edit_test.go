package edit

import (
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"unicode/utf16"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// repoFile reads a file relative to the repository root.
func repoFile(t *testing.T, parts ...string) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(append([]string{"..", "..", ".."}, parts...)...))
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func u32(v uint32) *uint32 { return &v }
func u8(v uint8) *uint8    { return &v }
func str(v string) *string { return &v }

// --- EQ graph -------------------------------------------------------------------------------

func TestLogAxisRoundTripAndClamps(t *testing.T) {
	for _, f := range []float64{10., 20., 100., 1000., 20000., 24000.} {
		if got := XFrequency(FrequencyX(f)); math.Abs(got-f) >= 1e-8 {
			t.Errorf("round trip of %v gave %v", f, got)
		}
	}
	if got := XFrequency(-1.); got != 10. {
		t.Errorf("XFrequency(-1) = %v", got)
	}
	if got := XFrequency(2.); got != 24000. {
		t.Errorf("XFrequency(2) = %v", got)
	}
}

// --- editor parameters ----------------------------------------------------------------------

func reported(latencySamples *uint32, liveLookahead *uint8) Reported {
	return Reported{LatencySamples: latencySamples, LiveLookahead: liveLookahead}
}

func TestADraftStatesTheLatencyItWouldBringFromTheMixersOwnReport(t *testing.T) {
	eq := func(got, want string) {
		t.Helper()
		if got != want {
			t.Errorf("got %q, want %q", got, want)
		}
	}
	// Mixer running LA0 reports 920 samples (19.2 ms) total (model + lead + one period).
	la0 := reported(u32(920), u8(0))
	eq(LookaheadFigure(la0, 0), "19.2 ms")
	eq(LookaheadFigure(la0, 1), "24.2 ms after applying")
	// Vice versa for LA1 at 1160 samples (24.2 ms).
	la1 := reported(u32(1160), u8(1))
	eq(LookaheadFigure(la1, 1), "24.2 ms")
	eq(LookaheadFigure(la1, 0), "19.2 ms after applying")
	// Each lookahead step is 240 samples.
	if v, ok := la0.AfterApplying(1); !ok || v != 1160 {
		t.Errorf("la0.AfterApplying(1) = %v, %v", v, ok)
	}
	if v, ok := la1.AfterApplying(0); !ok || v != 920 {
		t.Errorf("la1.AfterApplying(0) = %v, %v", v, ok)
	}
}

func TestWithoutAReportOnlyTheRelativeCostIsStated(t *testing.T) {
	eq := func(got, want string) {
		t.Helper()
		if got != want {
			t.Errorf("got %q, want %q", got, want)
		}
	}
	offline := reported(nil, nil)
	eq(LookaheadFigure(offline, 0), "")
	eq(LookaheadFigure(offline, 1), "")
	// Offline, the draft itself is the baseline.
	eq(LookaheadCost(offline, 0, 0), "now")
	eq(LookaheadCost(offline, 1, 0), "+5.0 ms")
	eq(LookaheadCost(offline, 0, 1), "\u22125.0 ms")
	// Live report overrides the draft as the baseline.
	live := reported(u32(920), u8(0))
	eq(LookaheadCost(live, 1, 1), "+5.0 ms")
	eq(LookaheadCost(live, 0, 0), "now")
	live = reported(u32(1160), u8(1))
	eq(LookaheadCost(live, 0, 1), "\u22125.0 ms")
	eq(LookaheadCost(live, 1, 0), "now")
	// Idle path (no latency figure) still reports relative cost.
	idle := reported(nil, u8(0))
	eq(LookaheadCost(idle, 1, 0), "+5.0 ms")
	eq(LookaheadFigure(idle, 1), "")
}

// --- show inputs ----------------------------------------------------------------------------

func inventory(name string) core.StockInventory {
	fader := uint8(1)
	return core.StockInventory{
		Available: true,
		ShowName:  "Show",
		Revision:  strings.Repeat("a", 64),
		Message:   "",
		Inputs: []core.StockSource{{
			ID:       "input-3",
			SourceID: 3,
			Fader:    &fader,
			Name:     name,
			Color:    "fffdba0e",
			TargetID: "mic-1",
		}},
	}
}

func TestAssignedPadsAreHiddenEvenInOldCachedInventory(t *testing.T) {
	cached := inventory("Mic 4")
	pads := cached.Inputs[0]
	pads.ID = "input-11"
	pads.SourceID = 11
	pads.Name = "SMART Pads"
	pads.TargetID = ""
	seven := uint8(7)
	pads.Fader = &seven
	cached.Inputs = append(cached.Inputs, pads)
	held := cached
	held.Inputs = slices.Clone(cached.Inputs)
	inputs := InventoryCache{Inventory: &held}
	if n := len(inputs.VisibleSources()); n != 1 {
		t.Fatalf("visible sources: %d", n)
	}
	if slices.ContainsFunc(inputs.VisibleSources(), func(s *core.StockSource) bool { return s.SourceID == 11 }) {
		t.Fatal("SMART Pads are visible")
	}
	if !inputs.Receive(cached, nil) {
		t.Fatal("receive refused the inventory")
	}
	if slices.ContainsFunc(inputs.VisibleSources(), func(s *core.StockSource) bool { return s.ID == "input-11" }) {
		t.Fatal("input-11 is visible")
	}
	if n := len(inputs.Inventory.Inputs); n != 2 {
		t.Fatalf("cached inputs: %d", n)
	}
}

func TestUnassignedSourcesDoNotLookLikeLiveOrAllocatedPaths(t *testing.T) {
	source := inventory("CallMe 1").Inputs[0]
	source.Fader = nil
	source.TargetID = ""
	if got := SourceDetail(&source, "Not routed"); got != "Unassigned" {
		t.Errorf("detail %q", got)
	}
	if got := AssignmentLabel(&source); got != "Not assigned to a fader" {
		t.Errorf("assignment %q", got)
	}
	source.TargetID = "callme-1"
	if got := SourceDetail(&source, "Not routed"); got != "Unassigned · Not routed" {
		t.Errorf("detail %q", got)
	}
}

func TestFailedRefreshRetainsLastConfirmedAssignmentUntilSuccess(t *testing.T) {
	var inputs InventoryCache
	if !inputs.Receive(inventory("Mic 4"), nil) {
		t.Fatal("first receive refused")
	}
	selected := str("input-3")
	inputs.Pending = true
	if inputs.Receive(core.StockInventory{}, errors.New("connection closed")) {
		t.Fatal("a failure replaced the inventory")
	}
	if inputs.Pending {
		t.Fatal("still pending")
	}
	if inputs.Inventory.Inputs[0].Name != "Mic 4" {
		t.Fatal("name changed")
	}
	if *selected != "input-3" {
		t.Fatal("selection changed")
	}
	if inputs.Error == nil || *inputs.Error != "connection closed" {
		t.Fatalf("error %v", inputs.Error)
	}
	unavailable := core.StockInventory{
		Available: false,
		Inputs:    nil,
		Revision:  "",
		Message:   "saved show unavailable",
		ShowName:  "",
	}
	if inputs.Receive(unavailable, nil) {
		t.Fatal("an unavailable show replaced the inventory")
	}
	if inputs.Inventory.Inputs[0].Name != "Mic 4" {
		t.Fatal("name changed")
	}
	if !inputs.Receive(inventory("Renamed Mic"), nil) {
		t.Fatal("receive refused")
	}
	if inputs.Inventory.Inputs[0].Name != "Renamed Mic" {
		t.Fatal("not renamed")
	}
	if inputs.Error != nil {
		t.Fatal("error kept")
	}
	// Explicit connection changes still construct a fresh cache.
	if (InventoryCache{}).Inventory != nil {
		t.Fatal("a fresh cache has an inventory")
	}
}

// --- selection ------------------------------------------------------------------------------

func TestPendingChangesIncludeOtherChannelsWhenTheEditorSwitches(t *testing.T) {
	saved := []string{"mic-1", "usb-main"}
	draft := []string{"mic-1", "headphones-1"}
	if got := ChangedPaths(saved, draft); !slices.Equal(got, []string{"usb-main", "headphones-1"}) {
		t.Errorf("changed %v", got)
	}
	if got := ChangedPaths(saved, saved); len(got) != 0 {
		t.Errorf("changed %v", got)
	}
}

// --- EQ import and export -------------------------------------------------------------------

func TestImportDetectsAutoEQTextAndNativeEasyEffectsWithoutAFormatPicker(t *testing.T) {
	options := core.ImportOptions{Stereo: true}
	apo := repoFile(t, "fixtures", "apo", "sennheiser-hd-6xx.txt")
	result, err := ParseImport(apo, options)
	if err != nil || result.Kind != core.ImportResultReady {
		t.Fatalf("apo: %v %v", result.Kind, err)
	}
	if len(result.EQ.Bands) != 10 || result.EQ.InputGainDB != -8.46 || result.EQ.Bands[0].Frequency != 105. {
		t.Fatalf("apo eq: %+v", result.EQ)
	}
	json := repoFile(t, "fixtures", "easyeffects", "output-split-different.json")
	result, err = ParseImport(json, options)
	if err != nil || result.Kind != core.ImportResultReady {
		t.Fatalf("json: %v %v", result.Kind, err)
	}
	if result.EQ.RightBands == nil {
		t.Fatal("right bands lost")
	}
	bytes := []byte{0xff, 0xfe}
	for _, u := range utf16.Encode([]rune(apo)) {
		bytes = append(bytes, byte(u), byte(u>>8))
	}
	if got, err := DecodeText(bytes); err != nil || got != apo {
		t.Fatalf("utf-16: %v", err)
	}
	if _, err := ParseImport("Filter 1: ON mystery", options); err == nil || !strings.Contains(err.Error(), "APO line 1") {
		t.Fatalf("mystery filter: %v", err)
	}
	if _, err := DecodeText([]byte{0xff, 0xfe, 1}); err == nil {
		t.Fatal("truncated UTF-16 accepted")
	}
}

func TestImportCannotReplaceAnotherTargetOrANewerEdit(t *testing.T) {
	target := core.DefaultConfig().Targets[4].Clone()
	captured := CaptureImportTarget(&target, 4)
	if !captured.Matches(&target, 4) {
		t.Fatal("capture does not match itself")
	}
	if captured.Matches(&target, 5) {
		t.Fatal("matches another generation")
	}
	other := target.Clone()
	other.ID = "different"
	if captured.Matches(&other, 4) {
		t.Fatal("matches another target")
	}
	if _, err := target.EQ.AddBand(core.ChannelLeft); err != nil {
		t.Fatal(err)
	}
	if captured.Matches(&target, 4) {
		t.Fatal("matches a newer edit")
	}
}

func TestNativeFileRoundtripPreservesSplitEQAndReplacesExportAtomically(t *testing.T) {
	directory := filepath.Join(os.TempDir(), fmt.Sprintf("rc2fx-eq-test-%d-%d", os.Getpid(), tempSequence.Add(1)-1))
	if err := os.Mkdir(directory, 0o777); err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(directory)
	path := filepath.Join(directory, "eq.json")
	text := repoFile(t, "fixtures", "easyeffects", "output-split-different.json")
	if err := WriteExport(path, text); err != nil {
		t.Fatal(err)
	}
	read, err := ReadPreset(path)
	if err != nil {
		t.Fatal(err)
	}
	result, err := core.ImportEasyEffects(read, core.ImportOptions{Stereo: true})
	if err != nil || result.Kind != core.ImportResultReady || result.EQ.RightBands == nil {
		t.Fatalf("expected split EQ: %v %v", result.Kind, err)
	}
	exported, err := core.ExportEasyEffects(result.EQ, core.PipelineOutput)
	if err != nil {
		t.Fatal(err)
	}
	if err := WriteExport(path, exported); err != nil {
		t.Fatal(err)
	}
	if read, err := ReadPreset(path); err != nil || read != exported {
		t.Fatalf("reread: %v", err)
	}
	entries, err := os.ReadDir(directory)
	if err != nil || len(entries) != 1 {
		t.Fatalf("directory holds %d entries: %v", len(entries), err)
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(directory); err != nil {
		t.Fatal(err)
	}
}

// --- settings files -------------------------------------------------------------------------

func TestImportPreservesLiveChannelOrderAndRefusesDifferentHardware(t *testing.T) {
	live := core.DefaultConfig()
	file := live.Clone()
	file.Revision = 999
	file.Targets[0].Gain.DB = -9.
	slices.Reverse(file.Targets)
	imported, err := ImportedTargets(file.Clone(), &live)
	if err != nil {
		t.Fatal(err)
	}
	if imported[0].ID != live.Targets[0].ID || imported[0].Gain.DB != -9. {
		t.Fatalf("first imported target: %s %v", imported[0].ID, imported[0].Gain.DB)
	}
	file.Targets = file.Targets[:len(file.Targets)-1]
	if _, err := ImportedTargets(file.Clone(), &live); err == nil {
		t.Fatal("a file for other hardware was accepted")
	}
	if _, err := ImportedTargets(file, nil); err != nil {
		t.Fatal(err)
	}
}

func TestFileSelectionCannotEraseLaterEditsOrAnotherConnection(t *testing.T) {
	session := core.DefaultSession()
	captured := CaptureDraft(&session, 3)
	if !captured.Matches(&session, 3) {
		t.Fatal("capture does not match itself")
	}
	if captured.Matches(&session, 4) {
		t.Fatal("matches another connection")
	}
	session.Draft[0].Gain.DB = -9.
	if captured.Matches(&session, 3) {
		t.Fatal("matches a later edit")
	}
	captured = CaptureDraft(&session, 3)
	session.SelectionDraft = append(session.SelectionDraft, "mic-1")
	if captured.Matches(&session, 3) {
		t.Fatal("matches a later selection")
	}
}
