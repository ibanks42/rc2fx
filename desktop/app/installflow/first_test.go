package installflow

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func TestTheFirstScreenNamesWhatTheAppSees(t *testing.T) {
	pro := installer.USBMixer{Model: firmware.ModelProIi, Serial: "GV1"}
	update := installer.USBMixer{Model: firmware.ModelProIi, Serial: "GV1", UpdateMode: true}
	duo := installer.USBMixer{Model: firmware.ModelDuo, Serial: "DU1"}
	for _, c := range []struct {
		why  string
		seen Sighting
		want First
	}{
		{"nothing", Sighting{}, FirstNothing},
		{"a Pro II without RC2FX", Sighting{USB: []installer.USBMixer{pro}}, FirstFound},
		{"a Duo without RC2FX", Sighting{USB: []installer.USBMixer{duo}}, FirstDuo},
		{"RC2FX's layout is live, so it is starting", Sighting{USB: []installer.USBMixer{pro}, RC2FX: true}, FirstStarting},
		{"just flashed and not seen: restarting", Sighting{Flashed: true}, FirstFinishing},
		{"just flashed, back in normal mode on RØDE's firmware", Sighting{USB: []installer.USBMixer{pro}, Flashed: true}, FirstFound},
		{"a Duo just flashed and back", Sighting{USB: []installer.USBMixer{duo}, Flashed: true}, FirstDuo},
		{"just flashed, RC2FX's layout live", Sighting{USB: []installer.USBMixer{pro}, RC2FX: true, Flashed: true}, FirstStarting},
		{"storage mode after a flash", Sighting{Storage: true, Flashed: true}, FirstStorage},
		{"update mode", Sighting{USB: []installer.USBMixer{update}}, FirstUpdateMode},
		{"update mode after a flash still waits for an update", Sighting{USB: []installer.USBMixer{update}, Flashed: true}, FirstUpdateMode},
		{"two mixers", Sighting{USB: []installer.USBMixer{pro, duo}}, FirstTwo},
		{"storage mode comes first", Sighting{Storage: true, USB: []installer.USBMixer{pro}}, FirstStorage},
	} {
		if got := FirstOf(c.seen); got != c.want {
			t.Errorf("%s: %v, want %v", c.why, got, c.want)
		}
	}
}

func TestAFlashCountsAsRecentOnlyUntilItSettles(t *testing.T) {
	saved := &installer.Saved{Commands: []installer.CommandRecord{{Command: "flash", AtUnix: 1000}}}
	if !FlashedRecently(saved, nil, 1060) {
		t.Fatal("a minute-old flash is recent")
	}
	if FlashedRecently(saved, nil, 1000+updateWindow) {
		t.Fatal("an old flash is recent")
	}
	settled := uint64(1000)
	if FlashedRecently(saved, &settled, 1060) {
		t.Fatal("a flash this app saw finish is recent")
	}
	saved.Commands = append(saved.Commands, installer.CommandRecord{Command: "usb-mode", AtUnix: 1050})
	if FlashedRecently(saved, nil, 1060) {
		t.Fatal("only a flash as the last command counts")
	}
	if FlashedRecently(nil, nil, 1060) {
		t.Fatal("nothing saved")
	}
}
