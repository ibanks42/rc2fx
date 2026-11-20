package installflow

// Math for the decorative onboarding signature waveform (drawn in
// app/installer_signature.go). Motion follows OS reduced-motion preferences,
// while the fill fraction reflects actual operation progress.

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

type SignatureKind int

const (
	SignatureLooking SignatureKind = iota
	SignatureReady
	SignatureWorking // Fill tracks progress of the active operation
	SignatureFailed  // Fill tracks how far the failed operation got
	SignatureComplete
)

// SignatureState describes what the signature renders. Fill is nil when
// progress is unknown or not applicable.
type SignatureState struct {
	Kind SignatureKind
	Fill *float32
}

// Fraction converts progress into a 0..1 fill ratio, or nil when Total is 0.
func Fraction(progress *installer.Progress) *float32 {
	if progress == nil || progress.Total == 0 {
		return nil
	}
	v := float32(min(max(float64(progress.Completed)/float64(progress.Total), 0), 1))
	return &v
}

// Offset computes the normalized waveform height at position u in [0, 1].
func Offset(u, phase float32, sample int, noise, voice float32) float32 {
	// Clamp before fractional pow since sin(pi) can dip slightly negative in float32.
	taper := num.Pow32(max(num.Sin32(math.Pi*u), 0), 1.2)
	word := num.Pow32(max(num.Sin32(2*math.Pi*(u*3.4-phase*0.12)), 0), 1.6)
	carrier := num.Sin32(u*140-phase*8)*0.6 + num.Sin32(u*61+phase*3)*0.4
	seed := num.Sin32((float32(sample)+num.Floor32(phase*30)*7)*12.9898) * 43758.547
	grain := (seed - num.Floor32(seed) - 0.5) * noise * 0.6
	return taper * (voice*word*carrier + grain)
}
