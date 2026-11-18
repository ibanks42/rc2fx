package core

import (
	"math"
	"strconv"
	"strings"
)

// Equalizer APO / AutoEQ parametric text. This parses data only: Include,
// expressions, channel routing and other commands are never executed or
// ignored.

func apoError(line int, message string) error {
	return CoreError("APO line " + strconv.Itoa(line) + ": " + message)
}

func apoNumber(text string, line int) (float64, error) {
	if value, ok := parseRustFloat(text); ok && !math.IsNaN(value) && !math.IsInf(value, 0) {
		return value, nil
	}
	return 0, apoError(line, "Expected a finite number, found "+text)
}

// asciiEqualFold compares a and b ignoring the case of ASCII letters only.
func asciiEqualFold(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := 0; i < len(a); i++ {
		x, y := a[i], b[i]
		if x >= 'A' && x <= 'Z' {
			x += 'a' - 'A'
		}
		if y >= 'A' && y <= 'Z' {
			y += 'a' - 'A'
		}
		if x != y {
			return false
		}
	}
	return true
}

func asciiUpper(text string) string {
	out := []byte(text)
	for i, c := range out {
		if c >= 'a' && c <= 'z' {
			out[i] = c - ('a' - 'A')
		}
	}
	return string(out)
}

// rustLines splits on \n, drops a \r before it, and gives no empty final
// line after a trailing newline.
func rustLines(text string) []string {
	if text == "" {
		return nil
	}
	lines := strings.Split(text, "\n")
	if lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	for i, line := range lines {
		lines[i] = strings.TrimSuffix(line, "\r")
	}
	return lines
}

// parseRustFloat reads a float strictly: an optional sign, then `inf`,
// `infinity` or `nan` in any case, or decimal digits with an optional point
// and exponent (`5.`, `.5` and `1e5` are numbers; hex and underscores are
// not).
func parseRustFloat(text string) (float64, bool) {
	body := text
	if body != "" && (body[0] == '+' || body[0] == '-') {
		body = body[1:]
	}
	switch strings.ToLower(body) {
	case "inf", "infinity", "nan":
		value, err := strconv.ParseFloat(text, 64)
		return value, err == nil
	}
	i, digits := 0, 0
	for i < len(body) && body[i] >= '0' && body[i] <= '9' {
		i++
		digits++
	}
	if i < len(body) && body[i] == '.' {
		i++
		for i < len(body) && body[i] >= '0' && body[i] <= '9' {
			i++
			digits++
		}
	}
	if digits == 0 {
		return 0, false
	}
	if i < len(body) && (body[i] == 'e' || body[i] == 'E') {
		i++
		if i < len(body) && (body[i] == '+' || body[i] == '-') {
			i++
		}
		exponentDigits := 0
		for i < len(body) && body[i] >= '0' && body[i] <= '9' {
			i++
			exponentDigits++
		}
		if exponentDigits == 0 {
			return 0, false
		}
	}
	if i != len(body) {
		return 0, false
	}
	value, err := strconv.ParseFloat(text, 64)
	if err != nil && !math.IsInf(value, 0) {
		// Underflow is a valid parse; overflow gives an infinity.
		if numError, ok := err.(*strconv.NumError); !ok || numError.Err != strconv.ErrRange {
			return 0, false
		}
	}
	return value, true
}

// ImportAPO imports up to 32 filters, keeping order, enabled
// state and exact parameter values. APO's preamp maps to EQ input gain and
// applies to both linked sides.
func ImportAPO(text string) (EQSettings, error) {
	if len(text) > MaxFileBytes {
		return EQSettings{}, CoreError("EQ preset exceeds 4 MiB")
	}
	eq := DefaultEQSettings()
	found := false
	for index, raw := range rustLines(strings.TrimLeft(text, "\ufeff")) {
		line := index + 1
		raw, _, _ = strings.Cut(raw, "#")
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		commandText, value, ok := strings.Cut(raw, ":")
		if !ok {
			return EQSettings{}, apoError(line, "Expected Preamp: or Filter: followed by parameters")
		}
		command := strings.Fields(commandText)
		words := strings.Fields(value)
		if len(command) == 1 && asciiEqualFold(command[0], "Preamp") {
			if len(words) != 2 || !asciiEqualFold(words[1], "dB") {
				return EQSettings{}, apoError(line, "Expected Preamp: <gain> dB")
			}
			gain, err := apoNumber(words[0], line)
			if err != nil {
				return EQSettings{}, err
			}
			eq.InputGainDB += gain
			found = true
			continue
		}
		if len(command) == 0 || !asciiEqualFold(command[0], "Filter") || len(command) > 2 {
			return EQSettings{}, apoError(line, "Only Preamp and parametric Filter commands are supported")
		}
		if len(command) == 2 {
			if _, ok := parseRustUint(command[1], 32); !ok {
				return EQSettings{}, apoError(line, "Only Preamp and parametric Filter commands are supported")
			}
		}
		if len(eq.Bands) == MaxBands {
			return EQSettings{}, apoError(line, "At most 32 filters are supported")
		}
		band := NewBand("band-" + strconv.Itoa(len(eq.Bands)+1))
		band.Mode = BandModeApoDr
		switch {
		case len(words) > 0 && asciiEqualFold(words[0], "ON"):
			band.Enabled = true
		case len(words) > 0 && asciiEqualFold(words[0], "OFF"):
			band.Enabled = false
		default:
			return EQSettings{}, apoError(line, "Filter must start with ON or OFF")
		}
		// REW also exports unused filters as just "Filter n: OFF".
		if !band.Enabled && len(words) == 1 {
			band.BandType = BandTypeOff
			eq.Bands = append(eq.Bands, band)
			found = true
			continue
		}
		if len(words) < 2 {
			return EQSettings{}, apoError(line, "Missing filter type")
		}
		kind := asciiUpper(words[1])
		switch kind {
		case "PK", "PEQ":
			band.BandType = BandTypeBell
		case "LSC", "LS":
			band.BandType = BandTypeLowShelf
		case "HSC", "HS":
			band.BandType = BandTypeHighShelf
		case "HP", "HPQ":
			band.BandType = BandTypeHighPass
		case "LP", "LPQ":
			band.BandType = BandTypeLowPass
		case "NO":
			band.BandType = BandTypeNotch
		default:
			return EQSettings{}, apoError(line, "Unsupported filter type "+kind)
		}
		var frequency, gain, q *float64
		for offset := 2; offset < len(words); {
			field := words[offset]
			switch {
			case asciiEqualFold(field, "Fc") || asciiEqualFold(field, "Gain"):
				if offset+1 >= len(words) {
					return EQSettings{}, apoError(line, "Missing parameter value")
				}
				if offset+2 >= len(words) {
					return EQSettings{}, apoError(line, "Missing Hz or dB unit")
				}
				v, unit := words[offset+1], words[offset+2]
				slot, expected := &gain, "dB"
				if asciiEqualFold(field, "Fc") {
					slot, expected = &frequency, "Hz"
				}
				if *slot != nil || !asciiEqualFold(unit, expected) {
					return EQSettings{}, apoError(line, "Repeated "+field+" or invalid unit (expected "+expected+")")
				}
				number, err := apoNumber(v, line)
				if err != nil {
					return EQSettings{}, err
				}
				*slot = &number
				offset += 3
			case asciiEqualFold(field, "Q"):
				if q != nil {
					return EQSettings{}, apoError(line, "Repeated Q parameter")
				}
				if offset+1 >= len(words) {
					return EQSettings{}, apoError(line, "Missing Q value")
				}
				number, err := apoNumber(words[offset+1], line)
				if err != nil {
					return EQSettings{}, err
				}
				q = &number
				offset += 2
			default:
				return EQSettings{}, apoError(line, "Unsupported parameter "+field+
					"; use Fc, Gain and Q (slope/BW variants are not supported)")
			}
		}
		if frequency == nil {
			return EQSettings{}, apoError(line, "Missing Fc frequency")
		}
		band.Frequency = *frequency
		hasGain := band.BandType == BandTypeBell || band.BandType == BandTypeLowShelf || band.BandType == BandTypeHighShelf
		switch {
		case hasGain && gain == nil:
			return EQSettings{}, apoError(line, "Missing Gain")
		case hasGain:
			band.Gain = *gain
		case gain != nil:
			return EQSettings{}, apoError(line, "This filter type does not accept Gain")
		default:
			band.Gain = 0.0
		}
		needsQ := kind == "PK" || kind == "PEQ" || kind == "HPQ" || kind == "LPQ" || kind == "NO"
		switch {
		case needsQ && q == nil:
			return EQSettings{}, apoError(line, "Missing Q")
		case q != nil:
			band.Q = *q
		default:
			band.Q = math.Sqrt2 / 2
		}
		// The engine clamps Q below .1 and frequencies above .49 * fs. Reject
		// such imports explicitly rather than silently changing their response.
		if err := rangeCheck(band.Q, 0.1, 100.0, "Q"); err != nil {
			return EQSettings{}, apoError(line, err.Error())
		}
		if err := rangeCheck(band.Frequency, 10.0, SampleRate*0.49, "Frequency"); err != nil {
			return EQSettings{}, apoError(line, err.Error())
		}
		if err := band.Validate(); err != nil {
			return EQSettings{}, apoError(line, err.Error())
		}
		eq.Bands = append(eq.Bands, band)
		found = true
	}
	if !found {
		return EQSettings{}, CoreError("No APO preamp or filters found")
	}
	if err := eq.Validate(2); err != nil {
		return EQSettings{}, err
	}
	return eq, nil
}
