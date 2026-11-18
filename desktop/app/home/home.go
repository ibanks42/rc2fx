// Package home is the pure parts of Home and of the opening screens, without
// gk: which channels the overview lists
// outside the show, and the Dusk halo's pixels.
package home

import (
	"math"
	"slices"
)

// ProcessedOffFaders is whether an input gets the overview's "not in this
// show" card: it is processed and no fader strip shows it. Removing a source
// from the show never hides a channel RC2FX still processes.
func ProcessedOffFaders(targetID string, processed bool, onFaders []string) bool {
	return processed && !slices.Contains(onFaders, targetID)
}

// The halo's design: two soft glows (blue left, violet right) around an
// invisible pill, at this window size.
var (
	HaloWindow = [2]float32{1320, 860}
	haloGlows  = [2]struct {
		color  uint32
		offset float32
	}{{0x7194c8, -140}, {0x8d7bc4, 140}}
)

const (
	HaloOpacity float32 = 0.065
	haloBlur    float32 = 220
	// HaloCell is window pixels per image pixel. A blur this wide has no fine
	// detail.
	HaloCell float32 = 8
)

// HaloSize is the halo image's size in pixels.
func HaloSize() (width, height int) {
	return int(math.Ceil(float64(HaloWindow[0] / HaloCell))), int(math.Ceil(float64(HaloWindow[1] / HaloCell)))
}

// haloGlow is a glow in the halo image: the pill, offset and blurred, as
// coverage from 0 to 1 for each image pixel.
func haloGlow(offset float32, width, height int) []float32 {
	windowWidth, windowHeight := HaloWindow[0], HaloWindow[1]
	left, right := 0.22*windowWidth+offset, 0.78*windowWidth+offset
	radius := 0.14 * windowHeight
	middle := 0.14*windowHeight + radius
	inside := func(xi, yi int) float32 {
		x, y := (float32(xi)+0.5)*HaloCell, (float32(yi)+0.5)*HaloCell
		dx := x - min(max(x, left+radius), right-radius)
		if float32(math.Hypot(float64(dx), float64(y-middle))) <= radius {
			return 1
		}
		return 0
	}
	sigma := haloBlur / HaloCell
	reach := int(3 * sigma)
	kernel := make([]float32, 0, 2*reach+1)
	var total float32
	for i := -reach; i <= reach; i++ {
		q := float32(i) / sigma
		w := float32(math.Exp(float64(-0.5 * (q * q))))
		kernel = append(kernel, w)
	}
	for _, w := range kernel {
		total += w
	}
	// One axis at a time: blurring in x then y is the same as blurring in
	// both.
	blur := func(source []float32, length int, at func(line, position int) int) []float32 {
		blurred := make([]float32, len(source))
		for line := 0; line < len(source)/length; line++ {
			for position := 0; position < length; position++ {
				var sum float32
				for k, weight := range kernel {
					i := position - reach + k
					if i >= 0 && i < length {
						sum += weight * source[at(line, i)]
					}
				}
				blurred[at(line, position)] = sum / total
			}
		}
		return blurred
	}
	mask := make([]float32, width*height)
	for i := range mask {
		mask[i] = inside(i%width, i/width)
	}
	rows := blur(mask, width, func(row, x int) int { return row*width + x })
	return blur(rows, height, func(column, y int) int { return y*width + column })
}

// HaloPixels is the halo's pixels, BGRA with straight alpha: the left glow
// under the right.
func HaloPixels(width, height int) []byte {
	color := func(value uint32) [3]float32 {
		return [3]float32{float32(value >> 16 & 0xff), float32(value >> 8 & 0xff), float32(value & 0xff)}
	}
	belowColor, aboveColor := color(haloGlows[0].color), color(haloGlows[1].color)
	below := haloGlow(haloGlows[0].offset, width, height)
	above := haloGlow(haloGlows[1].offset, width, height)
	pixels := make([]byte, 0, width*height*4)
	for i := range below {
		b, a := below[i]*HaloOpacity, above[i]*HaloOpacity
		alpha := a + b*(1-a)
		channel := func(c int) byte {
			mixed := aboveColor[c]*a + belowColor[c]*b*(1-a)
			return toU8(mixed / max(alpha, epsilon32))
		}
		pixels = append(pixels, channel(2), channel(1), channel(0), toU8(alpha*255))
	}
	return pixels
}

// epsilon32 is float32's machine epsilon, 2^-23.
const epsilon32 = 1.1920929e-07

// toU8 is v rounded half away from zero, saturating at 0 and 255.
func toU8(v float32) byte {
	r := math.Round(float64(v))
	switch {
	case r != r || r <= 0:
		return 0
	case r >= 255:
		return 255
	}
	return byte(r)
}
