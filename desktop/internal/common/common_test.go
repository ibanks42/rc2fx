package common

import (
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func TestLines(t *testing.T) {
	for _, c := range []struct {
		in   string
		want []string
	}{
		{"", nil},
		{"a", []string{"a"}},
		{"a\n", []string{"a"}},
		{"a\r\nb\r\n", []string{"a", "b"}},
		{"a\r\nb\r", []string{"a", "b\r"}},
		{"\n\n", []string{"", ""}},
	} {
		if got := Lines(c.in); !slices.Equal(got, c.want) {
			t.Errorf("Lines(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

func TestSplitLinesInclusive(t *testing.T) {
	if got := SplitLinesInclusive("a\nb\nc"); !slices.Equal(got, []string{"a\n", "b\n", "c"}) {
		t.Fatalf("got %q", got)
	}
	if got := SplitLinesInclusive("a\n"); !slices.Equal(got, []string{"a\n"}) {
		t.Fatalf("got %q", got)
	}
	if got := SplitLinesInclusive(""); len(got) != 0 {
		t.Fatalf("got %q", got)
	}
}

func TestIsHex(t *testing.T) {
	if !IsHex("") || !IsHex("0123456789abcdefABCDEF") {
		t.Fatal("valid hex rejected")
	}
	if IsHex("0g") || IsHex(" 0") {
		t.Fatal("invalid hex accepted")
	}
}

func TestIsSpace(t *testing.T) {
	for _, r := range []rune{' ', '\t', '\n', '\r', 0x85, 0xa0, 0x1680, 0x2000, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000} {
		if !IsSpace(r) {
			t.Errorf("IsSpace(%U) = false", r)
		}
	}
	for _, r := range []rune{'a', '0', 0x200b, 0xfeff} {
		if IsSpace(r) {
			t.Errorf("IsSpace(%U) = true", r)
		}
	}
}

func TestASCIIEqualFold(t *testing.T) {
	if !ASCIIEqualFold("PreAmp", "preamp") || ASCIIEqualFold("k", "\u212a") || ASCIIEqualFold("a", "ab") {
		t.Fatal("unexpected ASCIIEqualFold result")
	}
}

func TestParseUint(t *testing.T) {
	if v, ok := ParseUint("+42", 64); !ok || v != 42 {
		t.Fatalf("got %d, %v", v, ok)
	}
	for _, bad := range []string{"", "+", "-1", "1_0", "256"} {
		if _, ok := ParseUint(bad, 8); ok {
			t.Errorf("ParseUint(%q, 8) unexpectedly succeeded", bad)
		}
	}
}

func TestPathExistsAndSHA256Hex(t *testing.T) {
	path := filepath.Join(t.TempDir(), "f")
	if PathExists(path) {
		t.Fatal("missing path reported as existing")
	}
	if err := os.WriteFile(path, []byte("abc"), 0o600); err != nil {
		t.Fatal(err)
	}
	if !PathExists(path) {
		t.Fatal("existing path reported as missing")
	}
	if got := SHA256Hex([]byte("abc")); got != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
		t.Fatalf("SHA256Hex = %q", got)
	}
}
