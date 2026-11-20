package core

import (
	"strings"
	"testing"
)

// Show input assignments, release versions, SSH status and Equalizer APO
// import. rc2fx-core's inputs.rs, versions.rs and ssh.rs have unit tests of
// their own; APO import is Go only.

// Show inputs

func TestAssignmentsRequireUniquePhysicalSourcesAndProcessingBindings(t *testing.T) {
	inventory := StockInventory{
		Available: true,
		ShowName:  "Show",
		Revision:  strings.Repeat("a", 64),
		Message:   "",
		Inputs: []StockSource{{
			ID:       "input-3",
			SourceID: 3,
			Fader:    new(uint8(1)),
			Name:     "Mic 4",
			Color:    "fffdba0e",
			TargetID: "mic-1",
		}},
	}
	ok(t, inventory.Validate())
	other := inventory.Inputs[0]
	other.ID = "input-7"
	other.SourceID = 7
	other.Fader = new(uint8(2))
	inventory.Inputs = append(inventory.Inputs, other)
	fails(t, inventory.Validate())
	inventory.Inputs[1].TargetID = ""
	ok(t, inventory.Validate())
	inventory.Inputs[1].Fader = new(uint8(1))
	fails(t, inventory.Validate())
	inventory.Inputs[0].Fader = nil
	inventory.Inputs[1].Fader = nil
	ok(t, inventory.Validate())
	encoded := must(Encode(inventory))
	var decoded StockInventory
	ok(t, DecodeStrict(encoded, &decoded))
	check(t, deepEqual(decoded, inventory))
	inventory.Inputs[1].Fader = new(uint8(0))
	fails(t, inventory.Validate())
	inventory.Inputs[1].Fader = new(uint8(10))
	fails(t, inventory.Validate())
	inventory.Inputs = inventory.Inputs[:1]
	inventory.Available = false
	fails(t, inventory.Validate())
}

// Versions

func TestVersionsCompareNumericallyNotLexically(t *testing.T) {
	for _, c := range []struct {
		a, b string
		want int
	}{
		{"1.7.10", "1.7.9", 1},
		{"0.1.0", "0.2", -1},
		{"1.7", "1.7.0", 0},
		{"2", "10", -1},
	} {
		got, comparable := CompareVersions(c.a, c.b)
		check(t, comparable && got == c.want, c.a, c.b, got)
	}
}

func TestUnparseableVersionsAreNotComparable(t *testing.T) {
	for _, bad := range []string{"", "1..2", "1.2-rc1", "v1.2", " 1.2", "1.2.", "1234567890"} {
		_, comparable := CompareVersions(bad, "1.0")
		check(t, !comparable, bad)
		_, comparable = CompareVersions("1.0", bad)
		check(t, !comparable, bad)
	}
}

func TestTheShippedManifestParsesAndMatchesThisProtocol(t *testing.T) {
	release := must(ParseMixerRelease(repoFile(t, "config", "mixer-release.json")))
	check(t, release.ProtocolVersion == MixerProtocolVersion)
}

func TestManifestsAreChecked(t *testing.T) {
	check(t, must(ParseMixerRelease([]byte(`{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}`))).Version == "0.1.0")
	for _, bad := range []string{
		`{"schemaVersion":2,"version":"0.1.0","protocolVersion":1}`,
		`{"schemaVersion":1,"version":"latest","protocolVersion":1}`,
		`{"schemaVersion":1,"version":"0.1.0","protocolVersion":0}`,
		`{"schemaVersion":1,"version":"0.1.0","protocolVersion":1,"x":1}`,
		`{"schemaVersion":1,"version":"0.1.0","version":"0.2.0","protocolVersion":1}`,
		`{"schemaVersion":1,"version":"0.1.0"}`,
	} {
		fails(t, must2(ParseMixerRelease([]byte(bad))), bad)
	}
	_, err := ParseMixerRelease([]byte(`{"schemaVersion":1,"version":"latest","protocolVersion":1}`))
	check(t, err != nil && err.Error() == `mixer-release.json version "latest" is not dotted numeric`, err)
}

func TestDeviceVersionsOmitWhatIsUnknownAndTolerateNewFields(t *testing.T) {
	check(t, encode(t, DeviceVersions{}) == "{}")
	newer := must(FromJSON[DeviceVersions](`{"runtime":"0.2.0","protocol":2,"nativeProtocol":6,"firmware":"1.7.7","firmwareProfile":"rodecaster-pro-ii-1.7.7","future":true}`))
	check(t, newer.Runtime != nil && *newer.Runtime == "0.2.0")
	check(t, newer.NativeProtocol != nil && *newer.NativeProtocol == 6)
	check(t, newer.FirmwareProfile != nil && *newer.FirmwareProfile == "rodecaster-pro-ii-1.7.7")
}

// SSH

// A real Ed25519 public key, generated for these tests; its private half was
// discarded.
const testKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip"

func TestAPlainEd25519KeyIsAcceptedAndItsCommentDropped(t *testing.T) {
	key := must(ParseEd25519PublicKey(testKey + " me@laptop\n"))
	check(t, key.AsStr() == testKey)
}

func TestTheFingerprintMatchesSSHKeygen(t *testing.T) {
	// `ssh-keygen -lf` of this key printed this fingerprint.
	check(t, must(ParseEd25519PublicKey(testKey)).Fingerprint() == "SHA256:QbAgxtEX0z3ZCUzJ86NO6a0PYcnvrkRmVh0+cMm05TY")
}

func TestAnythingElseIsRefused(t *testing.T) {
	blob := testKey[len("ssh-ed25519 "):]
	for _, bad := range []string{
		"",
		"ssh-ed25519",
		"ssh-rsa " + blob,
		`command="sh" ` + testKey,
		"no-pty " + testKey,
		testKey + "\n" + testKey,
		testKey + "\rinjected",
		testKey + " \x1b[31m",
		"ssh-ed25519 " + blob[:len(blob)-4],
		"ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ==",
		"ssh-ed25519 !!!!",
		testKey + " " + strings.Repeat("x", MaxSSHPublicKey),
	} {
		fails(t, must2(ParseEd25519PublicKey(bad)), bad)
	}
}

func TestStatusTextIsCheckedBeforeItIsShownOrPinned(t *testing.T) {
	good := SSHStatus{
		Managed:            true,
		ManagedFingerprint: new("SHA256:abc"),
		HostKeys:           []string{testKey},
		Detail:             new("Running"),
	}
	ok(t, good.Validate())
	with := func(change func(*SSHStatus)) SSHStatus {
		status := good
		status.HostKeys = append([]string{}, good.HostKeys...)
		change(&status)
		return status
	}
	manyKeys := make([]string, maxHostKeys+1)
	for i := range manyKeys {
		manyKeys[i] = testKey
	}
	for i, bad := range []SSHStatus{
		with(func(s *SSHStatus) { s.HostKeys = []string{testKey + "\nssh-ed25519 other"} }),
		with(func(s *SSHStatus) { s.HostKeys = manyKeys }),
		with(func(s *SSHStatus) { s.HostKeys = []string{strings.Repeat("x", maxHostKey+1)} }),
		with(func(s *SSHStatus) { s.ManagedFingerprint = new("SHA256:\u202ecba") }),
		with(func(s *SSHStatus) { s.Detail = new(strings.Repeat("x", MaxStatusText+1)) }),
	} {
		fails(t, bad.Validate(), i)
	}
}

func TestStatusAndRequestRoundTrip(t *testing.T) {
	status := SSHStatus{
		Managed:            true,
		ManagedFingerprint: new("SHA256:abc"),
		UnmanagedKeys:      1,
		HostKeys:           []string{testKey},
		ServerRunning:      new(true),
	}
	text := encode(t, status)
	check(t, deepEqual(must(FromJSON[SSHStatus](text)), status))
	check(t, strings.Contains(text, `"unmanagedKeys":1`), text)
	// Newer fields are tolerated by an older reader...
	newer := must(FromJSON[SSHStatus](`{"managed":false,"unmanagedKeys":0,"hostKeys":[],"future":1}`))
	check(t, !newer.Managed)
	// ...but a reply missing what the status means is an error, never "off".
	for _, bad := range []string{
		"{}",
		"null",
		"[]",
		`{"managed":false}`,
		`{"managed":false,"unmanagedKeys":0}`,
		`{"unmanagedKeys":0,"hostKeys":[]}`,
	} {
		fails(t, must2(FromJSON[SSHStatus](bad)), bad)
	}
	request := must(FromJSON[SSHRequest](`{"enabled":true,"publicKey":"` + testKey + `"}`))
	check(t, request.Enabled)
	fails(t, must2(FromJSON[SSHRequest](`{"enabled":true,"x":1}`)))
}

// Equalizer APO import

func TestSuppliedAutoEQAndFifteenBandPresetsPreserveTheirCurves(t *testing.T) {
	for _, preset := range []struct {
		file   string
		count  int
		preamp float64
	}{
		{"sennheiser-hd-6xx.txt", 10, -8.46},
		{"fifteen-band.txt", 15, -15.5},
	} {
		eq := must(ImportAPO(string(repoFile(t, "fixtures", "apo", preset.file))))
		check(t, len(eq.Bands) == preset.count, preset.file)
		check(t, eq.InputGainDB == preset.preamp, preset.file, eq.InputGainDB)
		check(t, eq.OutputGainDB == 0.)
		check(t, eq.RightBands == nil)
		for _, band := range eq.Bands {
			check(t, band.Enabled && band.Mode == BandModeApoDr, band)
		}
		check(t, eq.Bands[0].BandType == BandTypeLowShelf)
		shelf := eq.Bands[0]
		center := MagnitudeDB(must(DesignCoefficients(shelf)), shelf.Frequency)
		check(t, approx(center, shelf.Gain/2., 1e-5), center)
		json := must(ExportEasyEffects(eq, PipelineOutput))
		restored := must(ImportEasyEffects(json, ImportOptions{Stereo: true}))
		check(t, restored.Kind == ImportResultReady)
		for _, hz := range []float64{20., 75., 105., 1000., 10000., 20000.} {
			check(t, approx(ResponseDB(eq, ChannelLeft, hz, true), ResponseDB(restored.EQ, ChannelRight, hz, true), 1e-9), hz)
		}
	}
	eq := must(ImportAPO(string(repoFile(t, "fixtures", "apo", "fifteen-band.txt"))))
	check(t, eq.Bands[0].Frequency == 21. && eq.Bands[0].Gain == 15.31 && eq.Bands[0].Q == 0.475, eq.Bands[0])
	check(t, eq.Bands[12].BandType == BandTypeHighShelf)
	check(t, eq.Bands[14].Frequency == 12178. && eq.Bands[14].Q == 0.643, eq.Bands[14])
}

func TestCommentsNumberingDisabledFiltersAndAdditivePreamp(t *testing.T) {
	eq := must(ImportAPO("\ufeff# REW\r\nPreamp: -2 dB\nPreamp: -1 dB\nFilter 99: OFF PK Fc 500 Hz Gain 3 dB Q 1 # retained\nFilter: ON HP Fc 40 Hz\nFilter 99: OFF"))
	check(t, eq.InputGainDB == -3.)
	check(t, len(eq.Bands) == 3)
	check(t, !eq.Bands[0].Enabled)
	check(t, eq.Bands[0].Gain == 3.)
	check(t, eq.Bands[1].BandType == BandTypeHighPass)
	check(t, eq.Bands[2].BandType == BandTypeOff)
}

func TestRejectsIncompleteUnsupportedAndSilentlyClampedSettings(t *testing.T) {
	for _, text := range []string{
		"",
		"Include: other.txt",
		"Channel: L",
		"Preamp: NaN dB",
		"Preamp: -100 dB",
		"Filter: ON PK Fc 50 Hz Gain 2 dB",
		"Filter: ON PK Fc 50 Hz Gain 2 dB Q 0.01",
		"Filter: ON PK Fc 24000 Hz Gain 2 dB Q 1",
		"Filter: ON LSC 6 dB Fc 100 Hz Gain 2 dB",
		"Filter: ON PK Fc 50 Hz Fc 60 Hz Gain 1 dB Q 1",
	} {
		fails(t, must2(ImportAPO(text)), text)
	}
	fails(t, must2(ImportAPO(strings.Repeat("Filter: ON HP Fc 40 Hz\n", 33))))
}
