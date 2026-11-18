package installflow

// The maths of the installer's signature (the Rust app's
// installer/signature.rs before): which picture it shows and the shape of its line. Package
// app draws it (installer_signature.go).
//
// Decorative onboarding signature, not audio or overall install progress.
// Decorative motion is driven by onboarding's Signature and the OS motion
// policy. Measured fill is driven solely by actual operation progress.

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// SignatureKind is what the signature shows.
type SignatureKind int

const (
	SignatureLooking SignatureKind = iota
	SignatureReady
	// SignatureWorking: Fill is how far the current operation is.
	SignatureWorking
	// SignatureFailed: Fill is how far the failed operation got.
	SignatureFailed
	SignatureComplete
)

// SignatureState is signature.rs State. Fill is nil when unknown (and for
// the kinds without one).
type SignatureState struct {
	Kind SignatureKind
	Fill *float32
}

// Fraction is how far one operation is: nil while its size is unknown,
// which is not zero.
func Fraction(progress *installer.Progress) *float32 {
	if progress == nil || progress.Total == 0 {
		return nil
	}
	v := float32(min(max(float64(progress.Completed)/float64(progress.Total), 0), 1))
	return &v
}

// The Rust computes in f32; Go has no float32 sin or pow, so these compute in
// float64 and round to float32 after each call. The result can differ from
// Rust's in the last bits, which the picture cannot show.
func sin32(x float32) float32    { return float32(math.Sin(float64(x))) }
func pow32(x, y float32) float32 { return float32(math.Pow(float64(x), float64(y))) }
func floor32(x float32) float32  { return float32(math.Floor(float64(x))) }

// Offset is the line's height at u (0 to 1 across), as a share of the
// amplitude, for sample i at phase.
func Offset(u, phase float32, sample int, noise, voice float32) float32 {
	// Same tapered, word-shaped carrier and grain as the approved HTML.
	// Clamp the taper before fractional powf (sin(pi) can be negative in f32).
	taper := pow32(max(sin32(math.Pi*u), 0), 1.2)
	word := pow32(max(sin32(2*math.Pi*(u*3.4-phase*0.12)), 0), 1.6)
	carrier := sin32(u*140-phase*8)*0.6 + sin32(u*61+phase*3)*0.4
	seed := sin32((float32(sample)+floor32(phase*30)*7)*12.9898) * 43758.547
	grain := (seed - floor32(seed) - 0.5) * noise * 0.6
	return taper * (voice*word*carrier + grain)
}
