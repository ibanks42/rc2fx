package app

import (
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

const testProfile = "rodecaster-pro-ii-1.7.7"

func signedOffer(version, minimumApp, profile string) installer.MixerOffer {
	return installer.MixerOffer{
		Target: "mixer-" + version + ".payload",
		SHA256: strings.Repeat("00", 32),
		Length: 1,
		Metadata: installer.MixerTarget{
			Version:           version,
			PayloadFormat:     1,
			ProtocolVersion:   uint32(core.MixerProtocolVersion),
			MinimumAppVersion: minimumApp,
			FirmwareProfiles:  []string{profile},
		},
	}
}

func connectedTo(runtime string, inPlace uint32, offers ...installer.MixerOffer) *App {
	a := &App{}
	a.session.Connected = true
	serial := "SER1"
	a.connectedMixer = &serial
	profile := testProfile
	a.deviceVersions = &core.DeviceVersions{Runtime: &runtime, FirmwareProfile: &profile, InPlaceUpdates: &inPlace}
	a.appUpdate.SetSigned(offers)
	return a
}

func TestTheNewestFittingSignedOfferIsTakenInPlace(t *testing.T) {
	a := connectedTo("0.2.1", 1,
		signedOffer("0.2.2", "0.0.1", testProfile),
		signedOffer("0.2.4", "0.0.1", "rodecaster-pro-ii-9.9.9"),
		signedOffer("0.2.3", "0.0.1", testProfile),
		signedOffer("0.3.0", "99.0.0", testProfile),
	)
	direct, ok := a.directInPlaceOffer()
	if !ok || direct.offer.Metadata.Version != "0.2.3" || direct.profile != testProfile || direct.serial != "SER1" {
		t.Fatalf("%v %+v", ok, direct)
	}
}

func TestNoInPlaceOfferWithoutSupportOrANewerVersion(t *testing.T) {
	newer := signedOffer("0.2.2", "0.0.1", testProfile)
	for name, a := range map[string]*App{
		"no in-place support": connectedTo("0.2.1", 0, newer),
		"already current":     connectedTo("0.2.2", 1, newer),
		"no signed offers":    connectedTo("0.2.1", 1),
	} {
		if _, ok := a.directInPlaceOffer(); ok {
			t.Errorf("%s: offered", name)
		}
	}
	a := connectedTo("0.2.1", 1, newer)
	a.session.Connected = false
	if _, ok := a.directInPlaceOffer(); ok {
		t.Error("offered while disconnected")
	}
}

func TestTheUpdateButtonNamesRØDEFirmwareOnlyWhenThatIsAllThatChanges(t *testing.T) {
	lines := func(mixer, firmware model.Tone) model.Overview {
		return model.Overview{Lines: []model.Line{
			{Part: model.PartApp, Tone: model.ToneQuiet},
			{Part: model.PartMixer, Tone: mixer},
			{Part: model.PartFirmware, Tone: firmware},
		}}
	}
	if !firmwareUpdate(lines(model.ToneQuiet, model.ToneUpdate)) {
		t.Fatal("firmware only")
	}
	if firmwareUpdate(lines(model.ToneUpdate, model.ToneUpdate)) || firmwareUpdate(lines(model.ToneUpdate, model.ToneQuiet)) {
		t.Fatal("mixer software changes too")
	}
}
