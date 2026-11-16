package core

import (
	"fmt"
	"slices"
	"strings"
	"testing"
)

const imageRecord = "schema=1\nstatus=repaired\nmanifest=abc\nchecked=2026-11-15T22:10:00Z\nrepaired=usr/bin/rode-init-update-env.sh\n"

func TestARecordForThisImageIsReported(t *testing.T) {
	check := ImageCheckFromRecord(ptrTo(imageRecord), ptrTo("abc"))
	if check.Status != ImageStatusRepaired {
		t.Fatalf("status %v", check.Status)
	}
	if !slices.Equal(check.Repaired, []string{"usr/bin/rode-init-update-env.sh"}) {
		t.Fatalf("repaired %q", check.Repaired)
	}
	if check.Checked == nil || *check.Checked != "2026-11-15T22:10:00Z" {
		t.Fatalf("checked %v", check.Checked)
	}
	json, err := ToValue(check)
	if err != nil {
		t.Fatal(err)
	}
	if status, _ := json.Get("status"); status.text != "repaired" {
		t.Fatalf("json status %v", status)
	}
	if _, ok := json.Get("damaged"); ok {
		t.Fatal("an empty list is left out")
	}
}

func TestNoListNoRecordOrAStaleRecord(t *testing.T) {
	cases := []struct {
		record, digest *string
		want           ImageStatus
	}{
		{ptrTo(imageRecord), nil, ImageStatusNone},
		{nil, ptrTo("abc"), ImageStatusPending},
		{ptrTo(imageRecord), ptrTo("other"), ImageStatusPending},
		{ptrTo("status=bogus\nmanifest=abc\n"), ptrTo("abc"), ImageStatusPending},
	}
	for i, c := range cases {
		if got := ImageCheckFromRecord(c.record, c.digest).Status; got != c.want {
			t.Errorf("case %d: %v, want %v", i, got, c.want)
		}
	}
}

func TestLongListsAreCapped(t *testing.T) {
	var record strings.Builder
	record.WriteString("status=damaged\nmanifest=abc\n")
	for n := range MaxListed + 3 {
		fmt.Fprintf(&record, "damaged=file%d\n", n)
	}
	check := ImageCheckFromRecord(ptrTo(record.String()), ptrTo("abc"))
	if len(check.Damaged) != MaxListed || check.Unlisted != 3 {
		t.Fatalf("%d listed, %d unlisted", len(check.Damaged), check.Unlisted)
	}
	// An older app reading a newer field set still parses.
	parsed, err := FromJSON[ImageCheck](`{"status":"ok","later":1}`)
	if err != nil || parsed.Status != ImageStatusOk {
		t.Fatalf("%+v, %v", parsed, err)
	}
}
