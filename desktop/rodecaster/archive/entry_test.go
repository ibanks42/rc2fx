package archive

import (
	"bytes"
	"compress/gzip"
	"testing"
)

func TestAppendLink(t *testing.T) {
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	w := &Writer{W: gz}
	for _, e := range []Entry{
		{Kind: EntryFile, Path: "etc/init.d/x", Mode: 0o755, Data: []byte("#!/bin/sh\n")},
		{Kind: EntryLink, Path: "etc/rc5.d/S02x", Mode: 0o777, Target: "../init.d/x"},
	} {
		if err := AppendEntry(w, e); err != nil {
			t.Fatal(err)
		}
	}
	if err := AppendEntry(w, Entry{Kind: EntryLink, Path: "bad", Mode: 0o777}); err == nil {
		t.Error("a link without a target was written")
	}
	if err := w.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	var got []*Member
	err := WalkReader(bytes.NewReader(buf.Bytes()), 1<<20, false, func(m *Member) error {
		c := *m
		got = append(got, &c)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 {
		t.Fatalf("%d members", len(got))
	}
	link := got[1]
	h := &link.Header
	if Normalized(link.Path) != "etc/rc5.d/S02x" || h.EntryType() != TypeSymlink || string(h.LinkName()) != "../init.d/x" || link.Size != 0 {
		t.Errorf("link member %q type %c target %q size %d", link.Path, h.EntryType(), h.LinkName(), link.Size)
	}
	for _, field := range [][2]int{{108, 116}, {116, 124}} {
		if v, err := Numeric(h[field[0]:field[1]]); err != nil || v != 0 {
			t.Errorf("owner field %v = %d, %v", field, v, err)
		}
	}
	if v, _ := Numeric(h[100:108]); v != 0o777 {
		t.Errorf("mode %o", v)
	}
	if got[0].Header.EntryType() != TypeRegular || got[0].Size != 10 {
		t.Errorf("file member %+v", got[0])
	}
}
