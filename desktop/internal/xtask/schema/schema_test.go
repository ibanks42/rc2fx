package schema

import (
	"strings"
	"testing"
)

func TestIdentifierConversion(t *testing.T) {
	cases := []struct {
		goName   string
		rustType string
		snake    string
		scream   string
	}{
		{"EQSettings", "EqSettings", "eq_settings", "EQ_SETTINGS"},
		{"USBStatus", "UsbStatus", "usb_status", "USB_STATUS"},
		{"USBLinkState", "UsbLinkState", "usb_link_state", "USB_LINK_STATE"},
		{"SSHStatus", "SshStatus", "ssh_status", "SSH_STATUS"},
		{"InputGainDB", "InputGainDb", "input_gain_db", "INPUT_GAIN_DB"},
		{"TargetIDs", "TargetIds", "target_ids", "TARGET_IDS"},
		{"WorkerCPU", "WorkerCpu", "worker_cpu", "WORKER_CPU"},
		{"USBGadget", "UsbGadget", "usb_gadget", "USB_GADGET"},
		{"USB1", "Usb1", "usb1", "USB1"},
		{"RMS", "Rms", "rms", "RMS"},
		{"DFNHopSamples", "DfnHopSamples", "dfn_hop_samples", "DFN_HOP_SAMPLES"},
		{"MaxSSHPublicKey", "MaxSshPublicKey", "max_ssh_public_key", "MAX_SSH_PUBLIC_KEY"},
	}
	for _, c := range cases {
		if got := toRustTypeName(c.goName); got != c.rustType {
			t.Errorf("toRustTypeName(%q) = %q, want %q", c.goName, got, c.rustType)
		}
		if got := toSnakeCase(c.goName); got != c.snake {
			t.Errorf("toSnakeCase(%q) = %q, want %q", c.goName, got, c.snake)
		}
		if got := toScreamingSnake(c.goName); got != c.scream {
			t.Errorf("toScreamingSnake(%q) = %q, want %q", c.goName, got, c.scream)
		}
	}
}

func TestGeneratedSchemaMatchesCheckedInFiles(t *testing.T) {
	if err := Run(true); err != nil {
		t.Fatal(err)
	}
}

func TestParseRejectsMissingJSONTag(t *testing.T) {
	src := []byte("package schema\ntype Bad struct { X int }\n")
	_, err := ParseFile("bad.go", src)
	if err == nil || !strings.Contains(err.Error(), "missing a json tag") {
		t.Fatalf("expected missing tag error, got %v", err)
	}
}
