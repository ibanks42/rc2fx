package main

import (
	"bytes"
	"image/png"
	"os"
	"testing"
)

func TestTheEmbeddedIconIsThePackagedIcon(t *testing.T) {
	packaged, err := os.ReadFile("../../packaging/rc2fx.png")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(icon, packaged) {
		t.Fatal("cmd/rc2fx/rc2fx.png differs from desktop/packaging/rc2fx.png: copy it again")
	}
	img, err := png.Decode(bytes.NewReader(icon))
	if err != nil {
		t.Fatal(err)
	}
	if b := img.Bounds(); b.Dx() != 64 || b.Dy() != 64 {
		t.Fatalf("icon is %v", b)
	}
	for y := 0; y < 64; y++ {
		for x := 0; x < 64; x++ {
			if _, _, _, a := img.At(x, y).RGBA(); a != 0 {
				return
			}
		}
	}
	t.Fatal("icon is fully transparent")
}
