package app

import (
	"bytes"
	"image/png"
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

func TestWindowsCarryThePackagedIcon(t *testing.T) {
	data, err := os.ReadFile("../packaging/rc2fx.png")
	if err != nil {
		t.Fatal(err)
	}
	icon, err := png.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if b := icon.Bounds(); b.Dx() != 64 || b.Dy() != 64 {
		t.Fatalf("icon is %v", b)
	}
	opaque := false
	for y := 0; y < 64 && !opaque; y++ {
		for x := 0; x < 64; x++ {
			if _, _, _, a := icon.At(x, y).RGBA(); a != 0 {
				opaque = true
				break
			}
		}
	}
	if !opaque {
		t.Fatal("icon is fully transparent")
	}
	// cmd/rc2fx embeds the same file and sets it before Run.
	SetWindowIcon(data)
	defer SetWindowIcon(nil)
	c := windowIdentity(gk.WindowConfig{Title: "RC2FX"})
	if c.AppID != "rc2fx" || !bytes.Equal(c.Icon, data) || c.Title != "RC2FX" {
		t.Fatalf("windowIdentity gives app id %q, %d icon bytes, title %q", c.AppID, len(c.Icon), c.Title)
	}
}
