package core

import "testing"

func TestAnOlderAppReadsANewerFieldSet(t *testing.T) {
	parsed, err := FromJSON[ImageCheck](`{"status":"ok","later":1}`)
	if err != nil || parsed.Status != ImageStatusOk {
		t.Fatalf("%+v, %v", parsed, err)
	}
}
