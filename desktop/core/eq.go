package core

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// Theoretical filter response, not an FFT or an audio processor.
//
// Products are wrapped in float64() where they feed a sum: that stops Go from
// fusing them into FMA instructions on arm64, so curves match the Rust on
// every platform.

type Coefficients struct {
	B0, B1, B2, A1, A2 float64
}

// CoefficientsIdentity passes audio through unchanged.
var CoefficientsIdentity = Coefficients{B0: 1.0}

func (c Coefficients) AsArray() [5]float64 { return [5]float64{c.B0, c.B1, c.B2, c.A1, c.A2} }

func DesignQ(band Band) float64 {
	switch {
	case band.Mode == BandModeRlcBt && band.BandType == BandTypeBell:
		g := math.Pow(10.0, band.Gain/20.0)
		return float64(float64(1.0+float64(2.0*band.Q))*math.Sqrt(g)) / float64(2.0*math.Sqrt(float64(1.0+float64(g*g))))
	case band.Mode == BandModeRlcBt:
		return (1.0 + band.Q) / 2.0
	}
	return num.Max(band.Q, 0.1)
}

func DesignCoefficients(band Band) (Coefficients, error) {
	if err := band.Validate(); err != nil {
		return Coefficients{}, err
	}
	if band.BandType == BandTypeOff {
		return CoefficientsIdentity, nil
	}
	w := float64(2.0*math.Pi) * num.Min(band.Frequency, float64(0.49*SampleRate)) / SampleRate
	c := math.Cos(w)
	alpha := math.Sin(w) / float64(2.0*DesignQ(band))
	a := math.Pow(10.0, band.Gain/40.0)
	beta := float64(2.0*math.Sqrt(a)) * alpha
	var n, d [3]float64
	switch band.BandType {
	case BandTypeBell:
		n = [3]float64{1.0 + float64(alpha*a), float64(-2.0 * c), 1.0 - float64(alpha*a)}
		d = [3]float64{1.0 + alpha/a, float64(-2.0 * c), 1.0 - alpha/a}
	case BandTypeLowPass:
		n = [3]float64{(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0}
		d = [3]float64{1.0 + alpha, float64(-2.0 * c), 1.0 - alpha}
	case BandTypeHighPass:
		n = [3]float64{(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0}
		d = [3]float64{1.0 + alpha, float64(-2.0 * c), 1.0 - alpha}
	case BandTypeNotch:
		n = [3]float64{1.0, float64(-2.0 * c), 1.0}
		d = [3]float64{1.0 + alpha, float64(-2.0 * c), 1.0 - alpha}
	case BandTypeLowShelf:
		amc := float64((a - 1.0) * c)
		apc := float64((a + 1.0) * c)
		n = [3]float64{
			float64(a * ((a + 1.0) - amc + beta)),
			float64(float64(2.0*a) * ((a - 1.0) - apc)),
			float64(a * ((a + 1.0) - amc - beta)),
		}
		d = [3]float64{
			(a + 1.0) + amc + beta,
			float64(-2.0 * ((a - 1.0) + apc)),
			(a + 1.0) + amc - beta,
		}
	case BandTypeHighShelf:
		amc := float64((a - 1.0) * c)
		apc := float64((a + 1.0) * c)
		n = [3]float64{
			float64(a * ((a + 1.0) + amc + beta)),
			float64(float64(-2.0*a) * ((a - 1.0) + apc)),
			float64(a * ((a + 1.0) + amc - beta)),
		}
		d = [3]float64{
			(a + 1.0) - amc + beta,
			float64(2.0 * ((a - 1.0) - apc)),
			(a + 1.0) - amc - beta,
		}
	default:
		return Coefficients{}, CoreError("Invalid filter coefficients")
	}
	value := Coefficients{
		B0: n[0] / d[0],
		B1: n[1] / d[0],
		B2: n[2] / d[0],
		A1: d[1] / d[0],
		A2: d[2] / d[0],
	}
	for _, v := range value.AsArray() {
		if math.IsNaN(v) || math.IsInf(v, 0) {
			return Coefficients{}, CoreError("Invalid filter coefficients")
		}
	}
	return value, nil
}

func MagnitudeDB(c Coefficients, frequency float64) float64 {
	w := float64(2.0*math.Pi) * frequency / SampleRate
	s1, c1 := math.Sin(w), math.Cos(w)
	s2, c2 := math.Sin(float64(2.0*w)), math.Cos(float64(2.0*w))
	nr := c.B0 + float64(c.B1*c1) + float64(c.B2*c2)
	ni := float64(c.B1*s1) + float64(c.B2*s2)
	dr := 1.0 + float64(c.A1*c1) + float64(c.A2*c2)
	di := float64(c.A1*s1) + float64(c.A2*s2)
	power := float64(float64(nr*nr)+float64(ni*ni)) / float64(float64(dr*dr)+float64(di*di))
	if !math.IsNaN(power) && !math.IsInf(power, 0) && power > 1e-30 {
		return 10.0 * math.Log10(power)
	}
	return -300.0
}

func activeBands(bands []Band) []Band {
	solo := false
	for _, band := range bands {
		if band.Solo {
			solo = true
		}
	}
	active := []Band{}
	for _, band := range bands {
		if band.Enabled && band.BandType != BandTypeOff && (!solo || band.Solo) {
			active = append(active, band)
		}
	}
	return active
}

func (e EQSettings) responseChannels() uint8 {
	if e.RightBands != nil {
		return 2
	}
	return 1
}

func CheckedResponseDB(eq EQSettings, channel Channel, frequency float64, includeGains bool) (float64, error) {
	if err := eq.Validate(eq.responseChannels()); err != nil {
		return 0, err
	}
	if err := rangeCheck(frequency, 0.0, SampleRate/2.0, "Response frequency"); err != nil {
		return 0, err
	}
	if !eq.Enabled {
		return 0.0, nil
	}
	value := 0.0
	if includeGains {
		value = eq.InputGainDB + eq.OutputGainDB
	}
	for _, band := range activeBands(eq.Channel(channel)) {
		coefficients, err := DesignCoefficients(band)
		if err != nil {
			return 0, err
		}
		value += MagnitudeDB(coefficients, frequency)
	}
	return value, nil
}

// ResponseDB is a convenience for a validated editor draft; invalid or
// transient edits plot flat.
func ResponseDB(eq EQSettings, channel Channel, frequency float64, includeGains bool) float64 {
	value, err := CheckedResponseDB(eq, channel, frequency, includeGains)
	if err != nil {
		return 0.0
	}
	return value
}

// ResponseCurve is ResponseDB at many frequencies, validating the EQ and
// designing each band once rather than once per point. Values match
// per-point calls.
func ResponseCurve(eq EQSettings, channel Channel, frequencies []float64, includeGains bool) []float64 {
	curve := make([]float64, len(frequencies))
	if eq.Validate(eq.responseChannels()) != nil || !eq.Enabled {
		return curve
	}
	bands := activeBands(eq.Channel(channel))
	designed := make([]Coefficients, 0, len(bands))
	for _, band := range bands {
		coefficients, err := DesignCoefficients(band)
		if err != nil {
			return make([]float64, len(frequencies))
		}
		designed = append(designed, coefficients)
	}
	offset := 0.0
	if includeGains {
		offset = eq.InputGainDB + eq.OutputGainDB
	}
	for i, frequency := range frequencies {
		if rangeCheck(frequency, 0.0, SampleRate/2.0, "Response frequency") != nil {
			curve[i] = 0.0
			continue
		}
		value := offset
		for _, c := range designed {
			value += MagnitudeDB(c, frequency)
		}
		curve[i] = value
	}
	return curve
}

func LogFrequencies(count int, min, max float64) []float64 {
	if count <= 0 || count > 65_536 || math.IsNaN(min) || math.IsInf(min, 0) ||
		math.IsNaN(max) || math.IsInf(max, 0) || min <= 0.0 || max < min {
		return []float64{}
	}
	if count == 1 {
		return []float64{min}
	}
	frequencies := make([]float64, count)
	for i := range frequencies {
		frequencies[i] = min * math.Pow(max/min, float64(i)/float64(count-1))
	}
	return frequencies
}
