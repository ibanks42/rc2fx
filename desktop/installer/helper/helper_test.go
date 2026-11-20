package helper

import (
	"os"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

func TestTheUSBRuleIsTheShippedOne(t *testing.T) {
	shipped, err := os.ReadFile("../../../config/udev/70-rc2fx.rules")
	if err != nil {
		t.Fatal(err)
	}
	if string(shipped) != USBRule {
		t.Fatal("installer/helper/70-rc2fx.rules differs from config/udev/70-rc2fx.rules")
	}
}

func TestRequestsAndCancelShareOneWireFormat(t *testing.T) {
	line, err := sjson.Marshal(inbound{Request: &installer.Request{Kind: installer.RequestReleases}})
	if err != nil {
		t.Fatal(err)
	}
	var back inbound
	if err := sjson.Unmarshal(line, &back); err != nil || back.Request == nil || back.Request.Kind != installer.RequestReleases {
		t.Fatalf("%s: %+v %v", line, back, err)
	}
	line, err = sjson.Marshal(inbound{Cancel: true})
	if err != nil {
		t.Fatal(err)
	}
	back = inbound{}
	if err := sjson.Unmarshal(line, &back); err != nil || !back.Cancel {
		t.Fatalf("%s: %+v %v", line, back, err)
	}
}

func TestRepliesAndProgressShareOneWireFormat(t *testing.T) {
	for _, m := range []message{
		{Progress: &installer.Progress{Phase: "download", Completed: 1, Total: 2, Detail: "x"}},
		{Done: &done{Reply: &installer.Reply{Kind: installer.ReplyDone}}},
		{Done: &done{Error: new("refused")}},
	} {
		line, err := sjson.Marshal(m)
		if err != nil {
			t.Fatal(err)
		}
		var back message
		if err := sjson.Unmarshal(line, &back); err != nil {
			t.Fatalf("%s: %v", line, err)
		}
		again, _ := sjson.Marshal(back)
		if string(again) != string(line) {
			t.Fatalf("%s became %s", line, again)
		}
	}
}
