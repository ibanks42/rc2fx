package installer

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

const (
	hostKey  = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip"
	hostKey2 = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG/gma/lTFUYuXY03/ZrYlH41SUBAkyyg1Z3s8EAqCRR"
)

func TestPinningReplacesOnlyThisMixersAlias(t *testing.T) {
	work := filepath.Join(t.TempDir(), "installer")
	must(t, os.MkdirAll(work, 0o755))
	other := "192.168.0.9 ssh-ed25519 AAAAOTHER\r\n# note\nlast-without-newline"
	put(t, filepath.Join(work, "known_hosts"), []byte(other))
	first, err := pinHostKeysIn(work, "SERIAL-1", []string{hostKey})
	must(t, err)
	if !strings.HasPrefix(first.Alias, "rc2fx-") || len(first.Alias) != 18 {
		t.Fatal(first.Alias)
	}
	second, err := pinHostKeysIn(work, "SERIAL-2", []string{hostKey})
	must(t, err)
	if first.Alias == second.Alias {
		t.Fatal("one alias for two mixers")
	}
	// Re-pinning SERIAL-1 replaces its line; SERIAL-2 and other lines stay
	// byte for byte, and other key types in the report are ignored.
	ecdsa := "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY="
	_, err = pinHostKeysIn(work, "SERIAL-1", []string{ecdsa, hostKey2})
	must(t, err)
	data, err := os.ReadFile(filepath.Join(work, "known_hosts"))
	must(t, err)
	text := string(data)
	if !strings.HasPrefix(text, "192.168.0.9 ssh-ed25519 AAAAOTHER\r\n# note\nlast-without-newline\n") {
		t.Fatalf("%q", text)
	}
	if strings.Count(text, first.Alias) != 1 || !strings.Contains(text, first.Alias+" "+hostKey2+"\n") ||
		!strings.Contains(text, second.Alias+" "+hostKey+"\n") || strings.Contains(text, "ecdsa") {
		t.Fatalf("%q", text)
	}
}

func TestConcurrentPinsKeepEveryMixer(t *testing.T) {
	work := t.TempDir()
	aliases := make([]string, 8)
	var wg sync.WaitGroup
	for i := range 8 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			pinned, err := pinHostKeysIn(work, fmt.Sprintf("SERIAL-%d", i), []string{hostKey})
			if err != nil {
				t.Error(err)
				return
			}
			aliases[i] = pinned.Alias
		}()
	}
	wg.Wait()
	data, err := os.ReadFile(filepath.Join(work, "known_hosts"))
	must(t, err)
	for _, alias := range aliases {
		if strings.Count(string(data), alias+" ") != 1 {
			t.Fatalf("%s: %q", alias, data)
		}
	}
}

func TestMalformedOrMissingHostKeysChangeNothing(t *testing.T) {
	work := t.TempDir()
	many := make([]string, 17)
	for i := range many {
		many[i] = hostKey
	}
	for _, bad := range [][]string{
		{},
		{"ssh-ed25519"},
		{hostKey + " comment"},
		{"ssh-dss AAAA"},
		{"ssh-ed25519 AAAA\nevil ssh-ed25519 AAAA"},
		{"ssh-ed25519 AA$A"},
		{"ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY="},
		{"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5"},
		many,
	} {
		if _, err := pinHostKeysIn(work, "S", bad); err == nil {
			t.Fatalf("%v accepted", bad)
		}
	}
	if _, err := pinHostKeysIn(work, "  ", []string{hostKey}); err == nil {
		t.Fatal("a blank serial was accepted")
	}
	if common.PathExists(filepath.Join(work, "known_hosts")) {
		t.Fatal("known_hosts was written")
	}
}
