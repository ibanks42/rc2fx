package installer

import (
	"bytes"
	"os/exec"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

func TestMixerTargetWireContract(t *testing.T) {
	accept := []struct {
		name string
		raw  string
	}{
		{"pro-ii-and-duo", `{"version":"0.2.1","payloadFormat":1,"protocolVersion":1,"minimumAppVersion":"0.1.7","firmwareProfiles":["rodecaster-pro-ii-1.7.7","rodecaster-duo-1.7.7"]}`},
	}
	for _, c := range accept {
		var target MixerTarget
		if err := sjson.DecodeStrict([]byte(c.raw), &target); err != nil {
			t.Fatalf("accept %s: unmarshal: %v", c.name, err)
		}
		if err := target.validate(); err != nil {
			t.Fatalf("accept %s: validate: %v", c.name, err)
		}
	}

	reject := []struct {
		name string
		raw  string
	}{
		{"unknown-field", `{"version":"0.2.1","payloadFormat":1,"protocolVersion":1,"minimumAppVersion":"0.1.7","firmwareProfiles":["rodecaster-pro-ii-1.7.7"],"extra":true}`},
		{"missing-minimum-app", `{"version":"0.2.1","payloadFormat":1,"protocolVersion":1,"firmwareProfiles":["rodecaster-pro-ii-1.7.7"]}`},
		{"empty-profiles", `{"version":"0.2.1","payloadFormat":1,"protocolVersion":1,"minimumAppVersion":"0.1.7","firmwareProfiles":[]}`},
	}
	for _, c := range reject {
		var target MixerTarget
		err := sjson.DecodeStrict([]byte(c.raw), &target)
		if err == nil {
			err = target.validate()
		}
		if err == nil {
			t.Errorf("reject %s: unexpectedly accepted %+v", c.name, target)
		}
	}
}

func TestPayloadEnvelopeContract(t *testing.T) {
	runEnvelope := func(extra ...string) []byte {
		args := append([]string{"run", "./internal/interop", "update-envelope"}, extra...)
		cmd := exec.Command("go", args...)
		cmd.Dir = ".."
		out, err := cmd.Output()
		if err != nil {
			t.Fatalf("interop update-envelope %v: %v", extra, err)
		}
		return out
	}

	valid := runEnvelope()
	env, err := embed.Find(valid)
	if err != nil {
		t.Fatalf("embed.Find(valid): %v", err)
	}
	bundle, err := env.Bundle()
	if err != nil {
		t.Fatalf("env.Bundle(): %v", err)
	}
	if len(bundle) != len(PayloadNames) {
		t.Fatalf("bundle has %d files, want %d", len(bundle), len(PayloadNames))
	}
	if !bytes.Contains(bundle["rc2fx.sh"], []byte("rc2fx_build=release\n")) {
		t.Fatalf("valid envelope rc2fx.sh missing release line")
	}

	devBytes := runEnvelope("--dev")
	devEnv, err := embed.Find(devBytes)
	if err != nil {
		t.Fatalf("embed.Find(dev): %v", err)
	}
	devBundle, err := devEnv.Bundle()
	if err != nil {
		t.Fatalf("devEnv.Bundle(): %v", err)
	}
	if bytes.Contains(devBundle["rc2fx.sh"], []byte("rc2fx_build=release\n")) {
		t.Errorf("dev-build unexpectedly marked release")
	}

	badMagic := bytes.Clone(valid)
	badMagic[0] = 'X'
	badDigest := bytes.Clone(valid)
	badDigest[len(badDigest)-1] ^= 0xff
	truncated := valid[:len(valid)-1]

	for _, c := range []struct {
		name string
		raw  []byte
	}{
		{"empty", nil},
		{"bad-magic", badMagic},
		{"bad-digest", badDigest},
		{"truncated", truncated},
	} {
		env, err := embed.Find(c.raw)
		if err == nil {
			_, err = env.Bundle()
		}
		if err == nil {
			t.Errorf("bad envelope %s was unexpectedly accepted", c.name)
		}
	}
}
