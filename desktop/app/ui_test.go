package app

import (
	"bytes"
	"os"
	"strings"
	"testing"

	"gpuikit.dev/gk"
)

// Windows match the packaged desktop identity and icon.
func TestWindowsMatchThePackagedDesktopIdentity(t *testing.T) {
	if appID != "rc2fx" {
		t.Fatalf("appID = %q", appID)
	}
	desktop, err := os.ReadFile("../packaging/rc2fx.desktop")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(desktop), "Icon=rc2fx\n") || !strings.Contains(string(desktop), "StartupWMClass=rc2fx\n") {
		t.Fatalf("the desktop entry does not name rc2fx:\n%s", desktop)
	}
}

// cmd/rc2fx embeds the packaged icon (its icon test checks the image) and
// sets it before Run; every window then carries it.
func TestWindowsCarryThePackagedIcon(t *testing.T) {
	data, err := os.ReadFile("../packaging/rc2fx.png")
	if err != nil {
		t.Fatal(err)
	}
	SetWindowIcon(data)
	defer SetWindowIcon(nil)
	c := windowIdentity(gk.WindowConfig{Title: "RC2FX"})
	if c.AppID != "rc2fx" || !bytes.Equal(c.Icon, data) || c.Title != "RC2FX" {
		t.Fatalf("windowIdentity gives app id %q, %d icon bytes, title %q", c.AppID, len(c.Icon), c.Title)
	}
}
