// Package sjson reads and writes JSON by serde's rules, which Go's
// encoding/json does not follow: field names match exactly,
// a struct field appears at most once, a missing field is an error unless
// the field or its struct has a default, `null` is only an absent Option,
// numbers keep their integer types, strings must be valid UTF-8 without
// lone surrogates, and nesting is bounded. Output matches serde_json's:
// compact or two-space pretty, struct fields in declaration order, map keys
// sorted, and only `"`, `\` and control characters escaped.
//
// Struct options come from tags. A blank field carries the container's
// serde attributes:
//
//	_ struct{} `serde:"default,deny_unknown_fields"`
//
// and each field's `json` tag gives its name and options: `default` (the
// field may be missing), `omitempty` (a nil pointer or interface is not
// written, serde's `skip_serializing_if = "Option::is_none"`) and `-`.
package sjson

import (
	"bytes"
	"fmt"
	"strconv"
	"unicode/utf16"
	"unicode/utf8"
)

// recursionLimit is serde_json's default nesting bound.
const recursionLimit = 128

type kind uint8

const (
	kindNull kind = iota
	kindBool
	kindNumber
	kindString
	kindArray
	kindObject
)

// node is one parsed JSON value. Objects keep every key in order, including
// duplicates, so struct decoding can refuse a repeated field.
type node struct {
	kind kind
	b    bool
	// s is a string's value or a number's literal text.
	s    string
	arr  []*node
	keys []string
	vals []*node
	raw  []byte
}

type parser struct {
	data  []byte
	at    int
	depth int
}

// parse reads exactly one JSON value surrounded by optional whitespace.
func parse(data []byte) (*node, error) {
	p := &parser{data: data}
	p.space()
	n, err := p.value()
	if err != nil {
		return nil, err
	}
	p.space()
	if p.at != len(p.data) {
		return nil, p.errorf("trailing characters")
	}
	return n, nil
}

func (p *parser) errorf(format string, args ...any) error {
	line, column := 1, 1
	for _, b := range p.data[:min(p.at, len(p.data))] {
		if b == '\n' {
			line++
			column = 1
		} else {
			column++
		}
	}
	return fmt.Errorf("%s at line %d column %d", fmt.Sprintf(format, args...), line, column)
}

func (p *parser) space() {
	for p.at < len(p.data) {
		switch p.data[p.at] {
		case ' ', '\t', '\n', '\r':
			p.at++
		default:
			return
		}
	}
}

func (p *parser) value() (*node, error) {
	if p.at >= len(p.data) {
		return nil, p.errorf("EOF while parsing a value")
	}
	start := p.at
	var n *node
	var err error
	switch c := p.data[p.at]; {
	case c == '{':
		n, err = p.object()
	case c == '[':
		n, err = p.array()
	case c == '"':
		var s string
		s, err = p.str()
		n = &node{kind: kindString, s: s}
	case c == 't':
		err = p.literal("true")
		n = &node{kind: kindBool, b: true}
	case c == 'f':
		err = p.literal("false")
		n = &node{kind: kindBool}
	case c == 'n':
		err = p.literal("null")
		n = &node{kind: kindNull}
	case c == '-' || (c >= '0' && c <= '9'):
		var s string
		s, err = p.number()
		n = &node{kind: kindNumber, s: s}
	default:
		return nil, p.errorf("expected value")
	}
	if err != nil {
		return nil, err
	}
	n.raw = p.data[start:p.at]
	return n, nil
}

func (p *parser) literal(word string) error {
	if !bytes.HasPrefix(p.data[p.at:], []byte(word)) {
		return p.errorf("expected ident")
	}
	p.at += len(word)
	return nil
}

func (p *parser) enter() error {
	p.depth++
	if p.depth > recursionLimit {
		return p.errorf("recursion limit exceeded")
	}
	return nil
}

func (p *parser) object() (*node, error) {
	if err := p.enter(); err != nil {
		return nil, err
	}
	defer func() { p.depth-- }()
	p.at++
	n := &node{kind: kindObject}
	p.space()
	if p.at < len(p.data) && p.data[p.at] == '}' {
		p.at++
		return n, nil
	}
	for {
		p.space()
		if p.at >= len(p.data) {
			return nil, p.errorf("EOF while parsing an object")
		}
		if p.data[p.at] != '"' {
			return nil, p.errorf("key must be a string")
		}
		key, err := p.str()
		if err != nil {
			return nil, err
		}
		p.space()
		if p.at >= len(p.data) || p.data[p.at] != ':' {
			return nil, p.errorf("expected `:`")
		}
		p.at++
		p.space()
		v, err := p.value()
		if err != nil {
			return nil, err
		}
		n.keys = append(n.keys, key)
		n.vals = append(n.vals, v)
		p.space()
		if p.at >= len(p.data) {
			return nil, p.errorf("EOF while parsing an object")
		}
		switch p.data[p.at] {
		case ',':
			p.at++
			p.space()
			if p.at < len(p.data) && p.data[p.at] == '}' {
				return nil, p.errorf("trailing comma")
			}
		case '}':
			p.at++
			return n, nil
		default:
			return nil, p.errorf("expected `,` or `}`")
		}
	}
}

func (p *parser) array() (*node, error) {
	if err := p.enter(); err != nil {
		return nil, err
	}
	defer func() { p.depth-- }()
	p.at++
	n := &node{kind: kindArray}
	p.space()
	if p.at < len(p.data) && p.data[p.at] == ']' {
		p.at++
		return n, nil
	}
	for {
		p.space()
		v, err := p.value()
		if err != nil {
			return nil, err
		}
		n.arr = append(n.arr, v)
		p.space()
		if p.at >= len(p.data) {
			return nil, p.errorf("EOF while parsing a list")
		}
		switch p.data[p.at] {
		case ',':
			p.at++
			p.space()
			if p.at < len(p.data) && p.data[p.at] == ']' {
				return nil, p.errorf("trailing comma")
			}
		case ']':
			p.at++
			return n, nil
		default:
			return nil, p.errorf("expected `,` or `]`")
		}
	}
}

func (p *parser) number() (string, error) {
	start := p.at
	if p.data[p.at] == '-' {
		p.at++
	}
	digits := func() int {
		n := 0
		for p.at < len(p.data) && p.data[p.at] >= '0' && p.data[p.at] <= '9' {
			p.at++
			n++
		}
		return n
	}
	if p.at >= len(p.data) {
		return "", p.errorf("EOF while parsing a value")
	}
	if p.data[p.at] == '0' {
		p.at++
		if p.at < len(p.data) && p.data[p.at] >= '0' && p.data[p.at] <= '9' {
			return "", p.errorf("invalid number")
		}
	} else if digits() == 0 {
		return "", p.errorf("invalid number")
	}
	if p.at < len(p.data) && p.data[p.at] == '.' {
		p.at++
		if digits() == 0 {
			return "", p.errorf("invalid number")
		}
	}
	if p.at < len(p.data) && (p.data[p.at] == 'e' || p.data[p.at] == 'E') {
		p.at++
		if p.at < len(p.data) && (p.data[p.at] == '+' || p.data[p.at] == '-') {
			p.at++
		}
		if digits() == 0 {
			return "", p.errorf("invalid number")
		}
	}
	return string(p.data[start:p.at]), nil
}

func (p *parser) str() (string, error) {
	p.at++
	var out []byte
	for {
		if p.at >= len(p.data) {
			return "", p.errorf("EOF while parsing a string")
		}
		c := p.data[p.at]
		switch {
		case c == '"':
			p.at++
			return string(out), nil
		case c == '\\':
			p.at++
			if p.at >= len(p.data) {
				return "", p.errorf("EOF while parsing a string")
			}
			e := p.data[p.at]
			p.at++
			switch e {
			case '"', '\\', '/':
				out = append(out, e)
			case 'b':
				out = append(out, '\b')
			case 'f':
				out = append(out, '\f')
			case 'n':
				out = append(out, '\n')
			case 'r':
				out = append(out, '\r')
			case 't':
				out = append(out, '\t')
			case 'u':
				r, err := p.hex4()
				if err != nil {
					return "", err
				}
				if utf16.IsSurrogate(r) {
					if r >= 0xdc00 {
						return "", p.errorf("lone leading surrogate in hex escape")
					}
					// As serde_json: no backslash after a leading surrogate is
					// a lone surrogate; a backslash without `u` ends the escape.
					if p.at >= len(p.data) || p.data[p.at] != '\\' {
						return "", p.errorf("lone leading surrogate in hex escape")
					}
					if p.at+1 >= len(p.data) || p.data[p.at+1] != 'u' {
						return "", p.errorf("unexpected end of hex escape")
					}
					p.at += 2
					low, err := p.hex4()
					if err != nil {
						return "", err
					}
					if low < 0xdc00 || low > 0xdfff {
						return "", p.errorf("lone leading surrogate in hex escape")
					}
					r = utf16.DecodeRune(r, low)
				}
				out = utf8.AppendRune(out, r)
			default:
				return "", p.errorf("invalid escape")
			}
		case c < 0x20:
			return "", p.errorf("control character (\\u0000-\\u001F) found while parsing a string")
		case c < 0x80:
			out = append(out, c)
			p.at++
		default:
			r, size := utf8.DecodeRune(p.data[p.at:])
			if r == utf8.RuneError && size <= 1 {
				return "", p.errorf("invalid unicode code point")
			}
			out = append(out, p.data[p.at:p.at+size]...)
			p.at += size
		}
	}
}

func (p *parser) hex4() (rune, error) {
	if p.at+4 > len(p.data) {
		return 0, p.errorf("EOF while parsing a string")
	}
	v, err := strconv.ParseUint(string(p.data[p.at:p.at+4]), 16, 32)
	if err != nil {
		return 0, p.errorf("invalid escape")
	}
	p.at += 4
	return rune(v), nil
}
