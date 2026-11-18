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
	"github.com/ibanks42/rc2fx/desktop/internal/num"
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

// Offset is the line's height at u (0 to 1 across), as a share of the
// amplitude, for sample i at phase.
func Offset(u, phase float32, sample int, noise, voice float32) float32 {
	// Same tapered, word-shaped carrier and grain as the approved HTML.
	// Clamp the taper before fractional powf (sin(pi) can be negative in f32).
	taper := num.Pow32(max(num.Sin32(math.Pi*u), 0), 1.2)
	word := num.Pow32(max(num.Sin32(2*math.Pi*(u*3.4-phase*0.12)), 0), 1.6)
	carrier := num.Sin32(u*140-phase*8)*0.6 + num.Sin32(u*61+phase*3)*0.4
	seed := num.Sin32((float32(sample)+num.Floor32(phase*30)*7)*12.9898) * 43758.547
	grain := (seed - num.Floor32(seed) - 0.5) * noise * 0.6
	return taper * (voice*word*carrier + grain)
}
