package core

import "math"

// Per-point reference implementation used to verify ResponseCurve.

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

// ResponseDB is CheckedResponseDB with an invalid draft plotting flat.
func ResponseDB(eq EQSettings, channel Channel, frequency float64, includeGains bool) float64 {
	value, err := CheckedResponseDB(eq, channel, frequency, includeGains)
	if err != nil {
		return 0.0
	}
	return value
}

// LogFrequencies is count points spaced evenly on a log scale from min to
// max.
func LogFrequencies(count int, min, max float64) []float64 {
	if count == 1 {
		return []float64{min}
	}
	frequencies := make([]float64, count)
	for i := range frequencies {
		frequencies[i] = min * math.Pow(max/min, float64(i)/float64(count-1))
	}
	return frequencies
}
