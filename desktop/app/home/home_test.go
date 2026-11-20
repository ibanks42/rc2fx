package home

import "testing"

func TestAProcessedInputWithoutAFaderStaysOnTheOverview(t *testing.T) {
	faders := []string{"usb-game", "usb-chat"}
	if !ProcessedOffFaders("mic-1", true, faders) {
		t.Fatal("a processed input off the faders vanished")
	}
	if ProcessedOffFaders("usb-game", true, faders) {
		t.Fatal("an input on a fader was listed twice")
	}
	if ProcessedOffFaders("mic-1", false, faders) {
		t.Fatal("an unprocessed input was listed")
	}
	if !ProcessedOffFaders("mic-1", true, nil) {
		t.Fatal("an empty show hid a processed input")
	}
}

func TestTheHaloIsFaintAndStrongestAroundTheMiddle(t *testing.T) {
	width, height := 165, 108
	pixels := HaloPixels(width, height)
	if len(pixels) != width*height*4 {
		t.Fatalf("%d bytes, want %d", len(pixels), width*height*4)
	}
	alpha := func(x, y int) byte { return pixels[(y*width+x)*4+3] }
	var peak byte
	for i := 3; i < len(pixels); i += 4 {
		peak = max(peak, pixels[i])
	}
	// Two glows at most, each no stronger than its opacity.
	if float32(peak) > 2*HaloOpacity*255 {
		t.Fatalf("peak alpha %d", peak)
	}
	center := alpha(width/2, height*28/100)
	if !(center > 0 && center > alpha(0, height-1)) {
		t.Fatalf("center alpha %d, corner %d", center, alpha(0, height-1))
	}
}

func TestEachSideOfTheHaloTakesItsOwnColor(t *testing.T) {
	width, height := 165, 108
	pixels := HaloPixels(width, height)
	blueToRed := func(x int) float32 {
		p := pixels[(height*28/100*width+x)*4:][:4]
		return float32(p[0]) / float32(p[2])
	}
	if !(blueToRed(width/5) > blueToRed(width*4/5)) {
		t.Fatalf("left %v, right %v", blueToRed(width/5), blueToRed(width*4/5))
	}
}

func TestTheHaloImageCoversTheDesignWindow(t *testing.T) {
	if w, h := HaloSize(); w != 165 || h != 108 {
		t.Fatalf("%d × %d", w, h)
	}
}
