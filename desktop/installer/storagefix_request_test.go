package installer

import (
	"encoding/json"
	"fmt"
	"testing"
)

// The storage fix's request carries Final and FixDebug through the helper;
// a request from an app that does not know FixDebug reads as false.
func TestStorageFixRequestRoundTrip(t *testing.T) {
	for _, want := range []Request{
		{Kind: RequestStorageFix},
		{Kind: RequestStorageFix, Final: true},
		{Kind: RequestStorageFix, Final: true, FixDebug: true},
	} {
		data, err := json.Marshal(want)
		if err != nil {
			t.Fatal(err)
		}
		var got Request
		if err := json.Unmarshal(data, &got); err != nil {
			t.Fatalf("%s: %v", data, err)
		}
		if got.Kind != want.Kind || got.Final != want.Final || got.FixDebug != want.FixDebug {
			t.Errorf("%s read back as %+v", data, got)
		}
	}
	var got Request
	raw := fmt.Sprintf(`{"kind":%d,"final":true}`, RequestStorageFix)
	if err := json.Unmarshal([]byte(raw), &got); err != nil || !got.Final || got.FixDebug {
		t.Fatalf("%+v, %v", got, err)
	}
}
