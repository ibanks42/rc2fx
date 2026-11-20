package state

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Parsing RC2FX_DEV_PAGE values.

func TestPagesAndChannelsParse(t *testing.T) {
	for _, c := range []struct {
		in   string
		want DevPage
	}{
		{"home", DevPage{Kind: DevPageHome}},
		{" Settings ", DevPage{Kind: DevPageSettings}},
		{"installer", DevPage{Kind: DevPageInstaller}},
		{"repair", DevPage{Kind: DevPageRepair}},
		{"channel:Mic 1:suppression", DevPage{Kind: DevPageChannel, Name: "Mic 1", Effect: core.EffectSuppression}},
		{"channel:headphones-1", DevPage{Kind: DevPageChannel, Name: "headphones-1"}},
	} {
		got, err := ParseDevPage(c.in, false)
		if err != nil || got != c.want {
			t.Errorf("parse(%q) = %+v, %v; want %+v", c.in, got, err, c.want)
		}
	}
}

func TestBadValuesAreRefused(t *testing.T) {
	for _, bad := range []string{"", "nope", "channel", "channel:", "channel:mic:reverb", "home:x"} {
		if _, err := ParseDevPage(bad, false); err == nil {
			t.Errorf("accepted %q", bad)
		}
	}
}

func TestChannelsMatchByNameOrIDIgnoringCase(t *testing.T) {
	channels := [][2]string{{"mic-1", "Mic 1"}, {"hp-1", "Headphones 1"}}
	if i, ok := FindChannel(channels, "mic 1"); !ok || i != 0 {
		t.Errorf("mic 1: %d %v", i, ok)
	}
	if i, ok := FindChannel(channels, "HP-1"); !ok || i != 1 {
		t.Errorf("HP-1: %d %v", i, ok)
	}
	if _, ok := FindChannel(channels, "Mic 2"); ok {
		t.Error("Mic 2 found")
	}
}

func TestFlowPagesAreForDevelopmentBuildsOnly(t *testing.T) {
	if got, err := ParseDevPage("flow:B4", false); err == nil {
		t.Fatalf("a release build accepted %+v", got)
	}
	got, err := ParseDevPage("flow:B4", true)
	if err != nil || got != (DevPage{Kind: DevPageFlow, Name: "B4"}) {
		t.Fatalf("parse = %+v, %v", got, err)
	}
	if _, err := ParseDevPage("flow:", true); err == nil {
		t.Fatal("accepted a flow page without a state")
	}
}
