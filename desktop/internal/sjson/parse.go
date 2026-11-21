package sjson

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"io"
	"math"
	"strconv"
	"strings"
)

// Default limits matching rc2fx-core's strict_json.
const (
	// DefaultMaxJSONBytes is the default maximum input size in bytes.
	DefaultMaxJSONBytes = 512 * 1024
	// DefaultMaxJSONNodes is the default maximum number of JSON values
	// (containers included, object keys excluded).
	DefaultMaxJSONNodes = 200_000
	// MaxJSONDepth is the maximum nesting depth (root is depth 0).
	MaxJSONDepth = 64
)

const (
	errJSONTooLarge = "JSON file is too large"
	errJSONTooDeep  = "JSON is too deeply nested or contains too many values"
)

// ParseJSON parses text with the default strict limits.
func ParseJSON(text string) (Tree, error) {
	return ParseJSONLimited(text, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
}

// ParseJSONLimited parses strict JSON with explicit byte and node bounds,
// rejecting duplicate keys at every depth and nesting deeper than MaxJSONDepth.
func ParseJSONLimited(text string, maximumBytes, maximumNodes int) (Tree, error) {
	return parseStrict([]byte(text), maximumBytes, maximumNodes)
}

// ParseJSONBytes is like ParseJSONLimited for a byte slice; non-UTF-8 input is
// rejected.
func ParseJSONBytes(data []byte, maximumBytes, maximumNodes int) (Tree, error) {
	return parseStrict(data, maximumBytes, maximumNodes)
}

// ParseJSONBytesLastKeyWins is like ParseJSONBytes, except that a key repeated
// in one object replaces the earlier value, as in serde_json's Value. It is for
// output from tools that repeat keys, such as cargo-about's report, which
// writes each target's "doctest" twice; input RC2FX owns stays strict.
func ParseJSONBytesLastKeyWins(data []byte, maximumBytes, maximumNodes int) (Tree, error) {
	return parse(data, maximumBytes, parser{
		maxNodes:    maximumNodes,
		maxDepth:    MaxJSONDepth,
		lastKeyWins: true,
		depthErr:    errJSONTooDeep,
	})
}

// SerdeRecursionLimit is serde_json's default nesting limit.
const SerdeRecursionLimit = 128

func parseStrict(data []byte, maximumBytes, maximumNodes int) (Tree, error) {
	return parseWithOptions(data, maximumBytes, maximumNodes, MaxJSONDepth, false, errJSONTooDeep)
}

func parseSerde(data []byte) (Tree, error) {
	return parseWithOptions(data, math.MaxInt, math.MaxInt, SerdeRecursionLimit-1, true, "recursion limit exceeded")
}

func parseWithOptions(data []byte, maximumBytes, maximumNodes, maximumDepth int, allowDupKeys bool, depthErr string) (Tree, error) {
	return parse(data, maximumBytes, parser{
		maxNodes:     maximumNodes,
		maxDepth:     maximumDepth,
		allowDupKeys: allowDupKeys,
		depthErr:     depthErr,
	})
}

func parse(data []byte, maximumBytes int, p parser) (Tree, error) {
	if len(data) > maximumBytes {
		return Tree{}, Error(errJSONTooLarge)
	}
	var opts []jsontext.Options
	if p.allowDupKeys || p.lastKeyWins {
		opts = append(opts, jsontext.AllowDuplicateNames(true))
	}
	p.data = data
	p.dec = jsontext.NewDecoder(bytes.NewReader(data), opts...)
	value, err := p.value(0)
	if err != nil {
		return Tree{}, err
	}
	if _, err := p.dec.ReadToken(); err != io.EOF {
		return Tree{}, Error("trailing characters")
	}
	return value, nil
}

type parser struct {
	data         []byte
	dec          *jsontext.Decoder
	nodes        int
	maxNodes     int
	maxDepth     int
	allowDupKeys bool
	// lastKeyWins keeps one member per key: a repeated key replaces the
	// earlier member's value.
	lastKeyWins bool
	depthErr    string
}

func (p *parser) rawSlice(raw jsontext.Value) []byte {
	end := int(p.dec.InputOffset())
	return p.data[end-len(raw) : end]
}

func (p *parser) value(depth int) (Tree, error) {
	p.nodes++
	if depth > p.maxDepth || p.nodes > p.maxNodes {
		return Tree{}, Error(p.depthErr)
	}
	switch kind := p.dec.PeekKind(); kind {
	case '[':
		if _, err := p.dec.ReadToken(); err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		start := int(p.dec.InputOffset()) - 1
		items := []Tree{}
		for p.dec.PeekKind() != ']' {
			item, err := p.value(depth + 1)
			if err != nil {
				return Tree{}, err
			}
			items = append(items, item)
		}
		if _, err := p.dec.ReadToken(); err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		end := int(p.dec.InputOffset())
		return Tree{kind: KindArray, items: items, raw: p.data[start:end]}, nil
	case '{':
		if _, err := p.dec.ReadToken(); err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		start := int(p.dec.InputOffset()) - 1
		members := []Member{}
		var index map[string]int
		for p.dec.PeekKind() != '}' {
			tok, err := p.dec.ReadToken()
			if err != nil {
				return Tree{}, wrapParseError(p.data, err)
			}
			if tok.Kind() != '"' {
				return Tree{}, Error("key must be a string")
			}
			key := tok.String()
			item, err := p.value(depth + 1)
			if err != nil {
				return Tree{}, err
			}
			if p.lastKeyWins && upsertMember(&members, &index, key, item) {
				continue
			}
			members = append(members, Member{Key: key, Value: item})
		}
		if _, err := p.dec.ReadToken(); err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		end := int(p.dec.InputOffset())
		return Tree{kind: KindObject, members: members, raw: p.data[start:end]}, nil
	case 'n', 't', 'f':
		raw, err := p.dec.ReadValue()
		if err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		out := NullTree()
		if kind != 'n' {
			out = BoolTree(kind == 't')
		}
		out.raw = p.rawSlice(raw)
		return out, nil
	case '"':
		raw, err := p.dec.ReadValue()
		if err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		rawSlice := p.rawSlice(raw)
		unquoted, err := jsontext.AppendUnquote(nil, rawSlice)
		if err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		out := StringTree(string(unquoted))
		out.raw = rawSlice
		return out, nil
	case '0':
		raw, err := p.dec.ReadValue()
		if err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		rawSlice := p.rawSlice(raw)
		number, err := parseNumber(rawSlice)
		if err != nil {
			return Tree{}, err
		}
		out := NumberTree(number)
		out.raw = rawSlice
		return out, nil
	default:
		if _, err := p.dec.ReadToken(); err != nil {
			return Tree{}, wrapParseError(p.data, err)
		}
		return Tree{}, Error("expected value")
	}
}

func upsertMember(members *[]Member, index *map[string]int, key string, value Tree) bool {
	if *index == nil {
		if len(*members) < 8 {
			for i := range *members {
				if (*members)[i].Key == key {
					(*members)[i].Value = value
					return true
				}
			}
			return false
		}
		*index = make(map[string]int, 16)
		for i, m := range *members {
			(*index)[m.Key] = i
		}
	}
	if i, ok := (*index)[key]; ok {
		(*members)[i].Value = value
		return true
	}
	(*index)[key] = len(*members)
	return false
}

func wrapParseError(data []byte, err error) error {
	if err == nil {
		return nil
	}
	var se Error
	if errors.As(err, &se) {
		return se
	}
	var serr *jsontext.SyntacticError
	if errors.As(err, &serr) {
		if errors.Is(serr.Err, jsontext.ErrDuplicateName) {
			return Error("Duplicate JSON key: " + serr.JSONPointer.LastToken())
		}
		if serr.ByteOffset >= 0 && int(serr.ByteOffset) < len(data) && data[serr.ByteOffset] == ',' {
			after := bytes.TrimLeft(data[serr.ByteOffset+1:], " \t\n\r")
			if len(after) > 0 && (after[0] == '}' || after[0] == ']') {
				return Error("trailing comma")
			}
		}
		msg := serr.Err.Error()
		if strings.HasPrefix(msg, "invalid surrogate pair ") {
			return Error("lone leading surrogate in hex escape")
		}
		if strings.HasPrefix(msg, "invalid escape sequence ") {
			if _, after, ok := strings.Cut(msg, `\u`); ok && len(after) >= 4 {
				if code, perr := strconv.ParseUint(after[:4], 16, 16); perr == nil && code >= 0xD800 && code <= 0xDFFF {
					return Error("lone leading surrogate in hex escape")
				}
			}
		}
	}
	return Error(err.Error())
}

// parseNumber parses a RFC 8259-validated JSON number identically to
// serde_json 1.0 without float_roundtrip (matching the mixer):
//
//   - Unsigned integers fit in u64, negative integers in i64; -0 and
//     out-of-range integers fall back to f64.
//   - Floats convert the first 19-20 decimal digits to f64 and scale by a
//     table power of ten. This is not always correctly rounded (e.g.
//     123456789012345678901 -> 1.2345678901234567e20), intentionally matching
//     the mixer bit for bit.
//   - Overflow to infinity returns "number out of range".
func parseNumber(raw []byte) (Number, error) {
	if !bytes.ContainsAny(raw, ".eE") {
		if raw[0] != '-' {
			if u, err := strconv.ParseUint(string(raw), 10, 64); err == nil {
				return NumberFromU64(u), nil
			}
		} else if len(raw) != 2 || raw[1] != '0' {
			if i, err := strconv.ParseInt(string(raw), 10, 64); err == nil {
				return Number{kind: numberI64, bits: uint64(i)}, nil
			}
		}
	}
	pos := 0
	positive := raw[0] != '-'
	if !positive {
		pos++
	}
	var significand uint64
	var exponent int32
	overflowed := false
	for pos < len(raw) && isDigit(raw[pos]) {
		digit := uint64(raw[pos] - '0')
		if overflowed {
			exponent++
		} else if significandOverflows(significand, digit) {
			overflowed = true
			exponent++
		} else {
			significand = significand*10 + digit
		}
		pos++
	}
	if pos < len(raw) && raw[pos] == '.' {
		pos++
		for pos < len(raw) && isDigit(raw[pos]) {
			digit := uint64(raw[pos] - '0')
			if !overflowed {
				if significandOverflows(significand, digit) {
					overflowed = true
				} else {
					significand = significand*10 + digit
					exponent--
				}
			}
			pos++
		}
	}
	if pos < len(raw) && (raw[pos] == 'e' || raw[pos] == 'E') {
		pos++
		positiveExp := true
		if raw[pos] == '+' || raw[pos] == '-' {
			positiveExp = raw[pos] == '+'
			pos++
		}
		var exp int32
		for pos < len(raw) {
			digit := int32(raw[pos] - '0')
			pos++
			if exp >= math.MaxInt32/10 && (exp > math.MaxInt32/10 || digit > math.MaxInt32%10) {
				if significand != 0 && positiveExp {
					return Number{}, Error("number out of range")
				}
				if positive {
					return Number{kind: numberF64, bits: math.Float64bits(0.0)}, nil
				}
				return Number{kind: numberF64, bits: math.Float64bits(math.Copysign(0, -1))}, nil
			}
			exp = exp*10 + digit
		}
		final := int64(exponent) - int64(exp)
		if positiveExp {
			final = int64(exponent) + int64(exp)
		}
		exponent = int32(max(math.MinInt32, min(math.MaxInt32, final)))
	}
	f, err := fromParts(positive, significand, exponent)
	if err != nil {
		return Number{}, err
	}
	return Number{kind: numberF64, bits: math.Float64bits(f)}, nil
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

// significandOverflows is serde_json's overflow!(s * 10 + d, u64::MAX).
func significandOverflows(significand, digit uint64) bool {
	const limit = math.MaxUint64
	return significand >= limit/10 && (significand > limit/10 || digit > limit%10)
}

// pow10 is serde_json's POW10 table: 1e0 through 1e308, each the correctly
// rounded literal.
var pow10 = func() (table [309]float64) {
	for i := range table {
		table[i], _ = strconv.ParseFloat("1e"+strconv.Itoa(i), 64)
	}
	return table
}()

func fromParts(positive bool, significand uint64, exponent int32) (float64, error) {
	f := float64(significand)
	e := int64(exponent)
	for {
		magnitude := e
		if magnitude < 0 {
			magnitude = -magnitude
		}
		if magnitude < int64(len(pow10)) {
			if e >= 0 {
				f *= pow10[magnitude]
				if math.IsInf(f, 0) {
					return 0, Error("number out of range")
				}
			} else {
				f /= pow10[magnitude]
			}
			break
		}
		if f == 0.0 {
			break
		}
		if e >= 0 {
			return 0, Error("number out of range")
		}
		f /= 1e308
		e += 308
	}
	if !positive {
		f = -f
	}
	return f, nil
}
