package sjson

import (
	"math"
	"strconv"
	"unicode/utf16"
	"unicode/utf8"
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
	p.data = data
	value, err := p.value(0)
	if err != nil {
		return Tree{}, err
	}
	p.skipSpace()
	if p.pos < len(p.data) {
		return Tree{}, p.fail("trailing characters")
	}
	return value, nil
}

type parser struct {
	data         []byte
	pos          int
	nodes        int
	maxNodes     int
	maxDepth     int
	allowDupKeys bool
	// lastKeyWins keeps one member per key: a repeated key replaces the
	// earlier member's value.
	lastKeyWins bool
	depthErr    string
}

// fail adds serde_json's position suffix: 1-based line, and the column of
// the offending byte (bytes since the line began).
func (p *parser) fail(message string) error {
	line, column := 1, 0
	end := p.pos
	if end > len(p.data) {
		end = len(p.data)
	}
	for _, b := range p.data[:end] {
		if b == '\n' {
			line++
			column = 0
		} else {
			column++
		}
	}
	if p.pos < len(p.data) {
		column++
	}
	return Error(message + " at line " + strconv.Itoa(line) + " column " + strconv.Itoa(column))
}

func (p *parser) skipSpace() {
	for p.pos < len(p.data) {
		switch p.data[p.pos] {
		case ' ', '\t', '\n', '\r':
			p.pos++
		default:
			return
		}
	}
}

func (p *parser) value(depth int) (Tree, error) {
	p.nodes++
	if depth > p.maxDepth || p.nodes > p.maxNodes {
		return Tree{}, p.fail(p.depthErr)
	}
	p.skipSpace()
	if p.pos >= len(p.data) {
		return Tree{}, p.fail("EOF while parsing a value")
	}
	start := p.pos
	var out Tree
	var err error
	switch c := p.data[p.pos]; {
	case c == 'n':
		err = p.literal("null")
	case c == 't':
		out, err = BoolTree(true), p.literal("true")
	case c == 'f':
		out, err = BoolTree(false), p.literal("false")
	case c == '"':
		p.pos++
		var text string
		text, err = p.stringBody()
		if err == nil {
			out = StringTree(text)
		}
	case c == '-' || (c >= '0' && c <= '9'):
		var number Number
		number, err = p.number()
		if err == nil {
			out = NumberTree(number)
		}
	case c == '[':
		p.pos++
		out, err = p.array(depth)
	case c == '{':
		p.pos++
		out, err = p.object(depth)
	default:
		return Tree{}, p.fail("expected value")
	}
	if err != nil {
		return Tree{}, err
	}
	out.raw = p.data[start:p.pos]
	return out, nil
}

func (p *parser) literal(word string) error {
	for i := 0; i < len(word); i++ {
		if p.pos >= len(p.data) {
			return p.fail("EOF while parsing a value")
		}
		if p.data[p.pos] != word[i] {
			if i == 0 {
				return p.fail("expected value")
			}
			return p.fail("expected ident")
		}
		p.pos++
	}
	return nil
}

func (p *parser) array(depth int) (Tree, error) {
	items := []Tree{}
	p.skipSpace()
	if p.pos < len(p.data) && p.data[p.pos] == ']' {
		p.pos++
		return Tree{kind: KindArray, items: items}, nil
	}
	for {
		item, err := p.value(depth + 1)
		if err != nil {
			return Tree{}, err
		}
		items = append(items, item)
		p.skipSpace()
		if p.pos >= len(p.data) {
			return Tree{}, p.fail("EOF while parsing a list")
		}
		switch p.data[p.pos] {
		case ',':
			p.pos++
			p.skipSpace()
			if p.pos < len(p.data) && p.data[p.pos] == ']' {
				return Tree{}, p.fail("trailing comma")
			}
		case ']':
			p.pos++
			return Tree{kind: KindArray, items: items}, nil
		default:
			return Tree{}, p.fail("expected `,` or `]`")
		}
	}
}

func (p *parser) object(depth int) (Tree, error) {
	members := []Member{}
	p.skipSpace()
	if p.pos < len(p.data) && p.data[p.pos] == '}' {
		p.pos++
		return Tree{kind: KindObject, members: members}, nil
	}
	// Track decoded keys for this object so escaped duplicates are caught.
	var seen map[string]struct{}
	// With lastKeyWins: each key's member, once the object has grown.
	var index map[string]int
	for {
		p.skipSpace()
		if p.pos >= len(p.data) {
			return Tree{}, p.fail("EOF while parsing an object")
		}
		if p.data[p.pos] != '"' {
			if p.data[p.pos] == '}' {
				return Tree{}, p.fail("trailing comma")
			}
			return Tree{}, p.fail("key must be a string")
		}
		p.pos++
		key, err := p.stringBody()
		if err != nil {
			return Tree{}, err
		}
		replace := -1
		if p.lastKeyWins {
			if len(members) < 8 && index == nil {
				for i := range members {
					if members[i].Key == key {
						replace = i
						break
					}
				}
			} else {
				if index == nil {
					index = make(map[string]int, 16)
					for i, member := range members {
						index[member.Key] = i
					}
				}
				if i, ok := index[key]; ok {
					replace = i
				} else {
					index[key] = len(members)
				}
			}
		} else if !p.allowDupKeys {
			duplicate := false
			if len(members) < 8 && seen == nil {
				for _, member := range members {
					if member.Key == key {
						duplicate = true
						break
					}
				}
			} else {
				if seen == nil {
					seen = make(map[string]struct{}, 16)
					for _, member := range members {
						seen[member.Key] = struct{}{}
					}
				}
				_, duplicate = seen[key]
				seen[key] = struct{}{}
			}
			if duplicate {
				return Tree{}, p.fail("Duplicate JSON key: " + key)
			}
		}
		p.skipSpace()
		if p.pos >= len(p.data) {
			return Tree{}, p.fail("EOF while parsing an object")
		}
		if p.data[p.pos] != ':' {
			return Tree{}, p.fail("expected `:`")
		}
		p.pos++
		item, err := p.value(depth + 1)
		if err != nil {
			return Tree{}, err
		}
		if replace >= 0 {
			members[replace].Value = item
		} else {
			members = append(members, Member{Key: key, Value: item})
		}
		p.skipSpace()
		if p.pos >= len(p.data) {
			return Tree{}, p.fail("EOF while parsing an object")
		}
		switch p.data[p.pos] {
		case ',':
			p.pos++
		case '}':
			p.pos++
			return Tree{kind: KindObject, members: members}, nil
		default:
			return Tree{}, p.fail("expected `,` or `}`")
		}
	}
}

// stringBody reads after the opening quote. Raw control characters, invalid
// escapes, lone surrogates and invalid UTF-8 are errors, as in serde_json.
func (p *parser) stringBody() (string, error) {
	start := p.pos
	// Fast path: no escapes.
	for p.pos < len(p.data) {
		c := p.data[p.pos]
		if c == '"' {
			raw := p.data[start:p.pos]
			if !utf8.Valid(raw) {
				return "", p.invalidUTF8(start)
			}
			p.pos++
			return string(raw), nil
		}
		if c == '\\' {
			break
		}
		if c < 0x20 {
			return "", p.fail("control character (\\u0000-\\u001F) found while parsing a string")
		}
		p.pos++
	}
	buffer := append([]byte(nil), p.data[start:p.pos]...)
	if !utf8.Valid(buffer) {
		return "", p.invalidUTF8(start)
	}
	for {
		if p.pos >= len(p.data) {
			return "", p.fail("EOF while parsing a string")
		}
		c := p.data[p.pos]
		switch {
		case c == '"':
			p.pos++
			return string(buffer), nil
		case c < 0x20:
			return "", p.fail("control character (\\u0000-\\u001F) found while parsing a string")
		case c == '\\':
			p.pos++
			if p.pos >= len(p.data) {
				return "", p.fail("EOF while parsing a string")
			}
			escape := p.data[p.pos]
			p.pos++
			switch escape {
			case '"', '\\', '/':
				buffer = append(buffer, escape)
			case 'b':
				buffer = append(buffer, '\b')
			case 'f':
				buffer = append(buffer, '\f')
			case 'n':
				buffer = append(buffer, '\n')
			case 'r':
				buffer = append(buffer, '\r')
			case 't':
				buffer = append(buffer, '\t')
			case 'u':
				r, err := p.unicodeEscape()
				if err != nil {
					return "", err
				}
				buffer = utf8.AppendRune(buffer, r)
			default:
				p.pos--
				return "", p.fail("invalid escape")
			}
		case c < utf8.RuneSelf:
			buffer = append(buffer, c)
			p.pos++
		default:
			r, size := utf8.DecodeRune(p.data[p.pos:])
			if r == utf8.RuneError && size <= 1 {
				return "", p.invalidUTF8(p.pos)
			}
			buffer = append(buffer, p.data[p.pos:p.pos+size]...)
			p.pos += size
		}
	}
}

func (p *parser) invalidUTF8(from int) error {
	for p.pos = from; p.pos < len(p.data); {
		r, size := utf8.DecodeRune(p.data[p.pos:])
		if r == utf8.RuneError && size <= 1 {
			break
		}
		p.pos += size
	}
	return p.fail("invalid unicode code point")
}

func (p *parser) hex4() (rune, error) {
	if p.pos+4 > len(p.data) {
		p.pos = len(p.data)
		return 0, p.fail("EOF while parsing a string")
	}
	var r rune
	for i := 0; i < 4; i++ {
		c := p.data[p.pos]
		var digit byte
		switch {
		case c >= '0' && c <= '9':
			digit = c - '0'
		case c >= 'a' && c <= 'f':
			digit = c - 'a' + 10
		case c >= 'A' && c <= 'F':
			digit = c - 'A' + 10
		default:
			return 0, p.fail("invalid escape")
		}
		r = r<<4 | rune(digit)
		p.pos++
	}
	return r, nil
}

func (p *parser) unicodeEscape() (rune, error) {
	first, err := p.hex4()
	if err != nil {
		return 0, err
	}
	switch {
	case first >= 0xDC00 && first <= 0xDFFF:
		return 0, p.fail("lone leading surrogate in hex escape")
	case first < 0xD800 || first > 0xDBFF:
		return first, nil
	}
	if p.pos+1 >= len(p.data) || p.data[p.pos] != '\\' || p.data[p.pos+1] != 'u' {
		if p.pos >= len(p.data) {
			return 0, p.fail("EOF while parsing a string")
		}
		return 0, p.fail("lone leading surrogate in hex escape")
	}
	p.pos += 2
	second, err := p.hex4()
	if err != nil {
		return 0, err
	}
	if second < 0xDC00 || second > 0xDFFF {
		return 0, p.fail("lone leading surrogate in hex escape")
	}
	return utf16.DecodeRune(first, second), nil
}

// number parses a JSON number identically to serde_json 1.0 without
// float_roundtrip (matching the mixer):
//
//   - Unsigned integers fit in u64, negative integers in i64; -0 and
//     out-of-range integers fall back to f64.
//   - Floats convert the first 19-20 decimal digits to f64 and scale by a
//     table power of ten. This is not always correctly rounded (e.g.
//     123456789012345678901 -> 1.2345678901234567e20), intentionally matching
//     the mixer bit for bit.
//   - Overflow to infinity returns "number out of range".
func (p *parser) number() (Number, error) {
	start := p.pos
	positive := true
	if p.data[p.pos] == '-' {
		positive = false
		p.pos++
	}
	if p.pos >= len(p.data) {
		return Number{}, p.fail("EOF while parsing a value")
	}
	c := p.data[p.pos]
	p.pos++
	switch {
	case c == '0':
		// There can be only one leading '0'.
		if p.pos < len(p.data) && isDigit(p.data[p.pos]) {
			return Number{}, p.fail("invalid number")
		}
		return p.numberTail(start, positive, 0)
	case c >= '1' && c <= '9':
		significand := uint64(c - '0')
		for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
			digit := uint64(p.data[p.pos] - '0')
			if significandOverflows(significand, digit) {
				return p.floatResult(start, p.longInteger(positive, significand))
			}
			p.pos++
			significand = significand*10 + digit
		}
		return p.numberTail(start, positive, significand)
	}
	p.pos--
	return Number{}, p.fail("invalid number")
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

// significandOverflows is serde_json's overflow!(s * 10 + d, u64::MAX).
func significandOverflows(significand, digit uint64) bool {
	const limit = math.MaxUint64
	return significand >= limit/10 && (significand > limit/10 || digit > limit%10)
}

type floatOutcome struct {
	value float64
	err   error
}

func (p *parser) floatResult(start int, outcome floatOutcome) (Number, error) {
	if outcome.err != nil {
		return Number{}, outcome.err
	}
	return Number{kind: numberF64, bits: math.Float64bits(outcome.value)}, nil
}

func (p *parser) numberTail(start int, positive bool, significand uint64) (Number, error) {
	if p.pos < len(p.data) {
		switch p.data[p.pos] {
		case '.':
			return p.floatResult(start, p.decimal(positive, significand, 0))
		case 'e', 'E':
			return p.floatResult(start, p.exponent(positive, significand, 0))
		}
	}
	if positive {
		return NumberFromU64(significand), nil
	}
	negated := -int64(significand) // wrapping, as Rust's wrapping_neg
	if negated >= 0 {
		// -0, and magnitudes beyond i64, are floats.
		return Number{kind: numberF64, bits: math.Float64bits(-float64(significand))}, nil
	}
	return Number{kind: numberI64, bits: uint64(negated)}, nil
}

func (p *parser) longInteger(positive bool, significand uint64) floatOutcome {
	var exponent int32
	for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
		p.pos++
		exponent++
	}
	if p.pos < len(p.data) {
		switch p.data[p.pos] {
		case '.':
			return p.decimal(positive, significand, exponent)
		case 'e', 'E':
			return p.exponent(positive, significand, exponent)
		}
	}
	return p.fromParts(positive, significand, exponent)
}

func (p *parser) decimal(positive bool, significand uint64, exponentBeforePoint int32) floatOutcome {
	p.pos++
	var exponentAfterPoint int32
	for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
		digit := uint64(p.data[p.pos] - '0')
		if significandOverflows(significand, digit) {
			for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
				p.pos++
			}
			exponent := exponentBeforePoint + exponentAfterPoint
			if p.pos < len(p.data) && (p.data[p.pos] == 'e' || p.data[p.pos] == 'E') {
				return p.exponent(positive, significand, exponent)
			}
			return p.fromParts(positive, significand, exponent)
		}
		p.pos++
		significand = significand*10 + digit
		exponentAfterPoint--
	}
	if exponentAfterPoint == 0 {
		if p.pos < len(p.data) {
			return floatOutcome{err: p.fail("invalid number")}
		}
		return floatOutcome{err: p.fail("EOF while parsing a value")}
	}
	exponent := exponentBeforePoint + exponentAfterPoint
	if p.pos < len(p.data) && (p.data[p.pos] == 'e' || p.data[p.pos] == 'E') {
		return p.exponent(positive, significand, exponent)
	}
	return p.fromParts(positive, significand, exponent)
}

func (p *parser) exponent(positive bool, significand uint64, startingExponent int32) floatOutcome {
	p.pos++
	positiveExponent := true
	if p.pos < len(p.data) {
		switch p.data[p.pos] {
		case '+':
			p.pos++
		case '-':
			p.pos++
			positiveExponent = false
		}
	}
	if p.pos >= len(p.data) {
		return floatOutcome{err: p.fail("EOF while parsing a value")}
	}
	if !isDigit(p.data[p.pos]) {
		return floatOutcome{err: p.fail("invalid number")}
	}
	exponent := int32(p.data[p.pos] - '0')
	p.pos++
	for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
		digit := int32(p.data[p.pos] - '0')
		p.pos++
		if exponent >= math.MaxInt32/10 && (exponent > math.MaxInt32/10 || digit > math.MaxInt32%10) {
			// Error instead of +/- infinity.
			if significand != 0 && positiveExponent {
				return floatOutcome{err: p.fail("number out of range")}
			}
			for p.pos < len(p.data) && isDigit(p.data[p.pos]) {
				p.pos++
			}
			if positive {
				return floatOutcome{value: 0.0}
			}
			return floatOutcome{value: math.Copysign(0, -1)}
		}
		exponent = exponent*10 + digit
	}
	final := int64(startingExponent) - int64(exponent)
	if positiveExponent {
		final = int64(startingExponent) + int64(exponent)
	}
	final = max(math.MinInt32, min(math.MaxInt32, final))
	return p.fromParts(positive, significand, int32(final))
}

// pow10 is serde_json's POW10 table: 1e0 through 1e308, each the correctly
// rounded literal.
var pow10 = func() (table [309]float64) {
	for i := range table {
		table[i], _ = strconv.ParseFloat("1e"+strconv.Itoa(i), 64)
	}
	return table
}()

func (p *parser) fromParts(positive bool, significand uint64, exponent int32) floatOutcome {
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
					return floatOutcome{err: p.fail("number out of range")}
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
			return floatOutcome{err: p.fail("number out of range")}
		}
		f /= 1e308
		e += 308
	}
	if !positive {
		f = -f
	}
	return floatOutcome{value: f}
}
