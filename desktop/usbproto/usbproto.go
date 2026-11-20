// Package usbproto implements RC2FX's control framing over vendor HID reports.
// Framing is stop-and-wait with no unsolicited writes or automatic mutation
// replay.
//
// This is a Go port of crates/rc2fx-usb (still used on the mixer); both sides
// share test fixtures under fixtures/usb.
package usbproto

import (
	"bytes"
	"compress/flate"
	"compress/zlib"
	"encoding/binary"
	"errors"
	"io"
	"math"
	"strconv"
)

const (
	ReportSize = 256
	Header     = 32
	Payload    = ReportSize - Header
	OutputID   = 5
	// RC2FX adds report IDs 5 and 6 to RODE's top-level collection (usage page
	// ff00, which uses reports 1-4). Any reader on that collection will see
	// input report 6 and must ignore it; RODE's own software filters by ID.
	InputID       = 6
	MaxRequest    = 512*1024 + 1024
	MaxReply      = 8*1024*1024 + 1024
	MaxEventBytes = 512 * 1024
	MaxEventReply = MaxEventBytes + 1024 // event payload plus envelope
	Magic         = "\xffRC2FX\x00"
	Hello3        = "RC2FX USB 3"
	// Hello4 is sent by protocol 4 clients; the mixer replies with "RC2FX USB 4 <window>".
	Hello4 = "RC2FX USB 4"
	// MaxWindow caps how many reports a client may keep in flight. The mixer's
	// native hook forwards reports via non-blocking datagram sends, so exceeding
	// the kernel's max_dgram_qlen would silently drop frames.
	MaxWindow = 32
)

// Hello4Window parses the window size from a protocol 4 hello ack
// ("RC2FX USB 4 <window>"). A bare "RC2FX USB 4" returns false.
func Hello4Window(data []byte) (int, bool) {
	digits, ok := bytes.CutPrefix(data, []byte("RC2FX USB 4 "))
	if !ok || len(digits) == 0 {
		return 0, false
	}
	for _, b := range digits {
		if b < '0' || b > '9' {
			return 0, false
		}
	}
	window, err := strconv.ParseUint(string(digits), 10, 64)
	if err != nil || window < 1 || window > 64 {
		return 0, false
	}
	return int(window), true
}

// Kind identifies a frame's message type.
type Kind uint8

const (
	KindHello   Kind = 1
	KindRequest Kind = 2
	KindFetch   Kind = 3
	KindAck     Kind = 4
	KindReply   Kind = 5
	KindError   Kind = 6
)

func (k Kind) String() string {
	switch k {
	case KindHello:
		return "Hello"
	case KindRequest:
		return "Request"
	case KindFetch:
		return "Fetch"
	case KindAck:
		return "Ack"
	case KindReply:
		return "Reply"
	case KindError:
		return "Error"
	}
	return "Kind(" + strconv.Itoa(int(k)) + ")"
}

// Frame is a decoded HID report. Data aliases the underlying report buffer.
type Frame struct {
	Kind    Kind
	Session uint64
	Request uint32
	Offset  int
	Total   int
	Data    []byte
}

// Marked reports whether bytes carries the RC2FX magic for the given report ID.
// This check intentionally runs before full validation so malformed RC2FX
// packets are never passed through to RODE's command parser.
func Marked(bytes []byte, report uint8) bool {
	return len(bytes) >= 8 && bytes[0] == report && string(bytes[1:8]) == Magic
}

// DeclaresControl inspects a HID report descriptor to check whether RC2FX's
// input and output report IDs are declared (purely passive, sends nothing).
func DeclaresControl(descriptor []byte) bool {
	var report uint32
	var stack []uint32
	output, input := false, false
	i := 0
	for i < len(descriptor) {
		prefix := descriptor[i]
		i++
		if prefix == 0xfe {
			if i >= len(descriptor) {
				return false
			}
			size := int(descriptor[i])
			i += 2 + size
			continue
		}
		n := int(prefix & 3)
		if n == 3 {
			n = 4
		}
		if i+n > len(descriptor) {
			return false
		}
		item := descriptor[i : i+n]
		i += n
		var value uint32
		for j := len(item) - 1; j >= 0; j-- {
			value = value<<8 | uint32(item[j])
		}
		switch prefix & 0xfc {
		case 0x84:
			report = value
		case 0xa4:
			stack = append(stack, report)
		case 0xb4:
			if len(stack) == 0 {
				return false
			}
			report = stack[len(stack)-1]
			stack = stack[:len(stack)-1]
		case 0x80:
			input = input || report == InputID
		case 0x90:
			output = output || report == OutputID
		}
	}
	return output && input
}

// DecodeFrame decodes one report with the given ID.
func DecodeFrame(bytes []byte, report uint8) (Frame, bool) {
	if len(bytes) != ReportSize || !Marked(bytes, report) || bytes[8] != 3 {
		return Frame{}, false
	}
	kind := Kind(bytes[9])
	if kind < KindHello || kind > KindError {
		return Frame{}, false
	}
	n := int(binary.LittleEndian.Uint16(bytes[30:32]))
	if n > Payload {
		return Frame{}, false
	}
	return Frame{
		Kind:    kind,
		Session: binary.LittleEndian.Uint64(bytes[10:18]),
		Request: binary.LittleEndian.Uint32(bytes[18:22]),
		Offset:  int(binary.LittleEndian.Uint32(bytes[22:26])),
		Total:   int(binary.LittleEndian.Uint32(bytes[26:30])),
		Data:    bytes[Header : Header+n],
	}, true
}

// Encode serializes f into a fixed-size HID report. Panics if Data exceeds
// Payload or if Offset/Total don't fit in uint32.
func (f *Frame) Encode(report uint8) [ReportSize]byte {
	if len(f.Data) > Payload || f.Offset < 0 || uint64(f.Offset) > math.MaxUint32 ||
		f.Total < 0 || uint64(f.Total) > math.MaxUint32 {
		panic("usbproto: frame does not fit a report")
	}
	var bytes [ReportSize]byte
	bytes[0] = report
	copy(bytes[1:8], Magic)
	bytes[8] = 3
	bytes[9] = uint8(f.Kind)
	binary.LittleEndian.PutUint64(bytes[10:18], f.Session)
	binary.LittleEndian.PutUint32(bytes[18:22], f.Request)
	binary.LittleEndian.PutUint32(bytes[22:26], uint32(f.Offset))
	binary.LittleEndian.PutUint32(bytes[26:30], uint32(f.Total))
	binary.LittleEndian.PutUint16(bytes[30:32], uint16(len(f.Data)))
	copy(bytes[Header:], f.Data)
	return bytes
}

var (
	errTooLarge        = errors.New("USB message too large")
	errDecodedTooLarge = errors.New("Decoded USB message too large")
	errInvalidZ        = errors.New("Invalid compressed USB message")
	errEncoding        = errors.New("Invalid USB message encoding")
	// Matches flate2's error message on the Rust side.
	errInflate = errors.New("deflate decompression error")
)

// Pack encodes a JSON message, compressing with zlib when that saves space.
// Unpack enforces the declared decoded size to reject zip bombs, truncation,
// and trailing bytes.
func Pack(json []byte, limit int) ([]byte, error) {
	if len(json) > limit || uint64(len(json)) > math.MaxUint32 {
		return nil, errTooLarge
	}
	var compressed bytes.Buffer
	encoder, err := zlib.NewWriterLevel(&compressed, zlib.BestSpeed)
	if err != nil {
		return nil, err
	}
	if _, err := encoder.Write(json); err != nil {
		return nil, err
	}
	if err := encoder.Close(); err != nil {
		return nil, err
	}
	var encoded []byte
	if compressed.Len()+5 < len(json)+1 {
		encoded = make([]byte, 0, 5+compressed.Len())
		encoded = append(encoded, 'Z')
		encoded = binary.LittleEndian.AppendUint32(encoded, uint32(len(json)))
		encoded = append(encoded, compressed.Bytes()...)
	} else {
		encoded = make([]byte, 0, 1+len(json))
		encoded = append(encoded, 'J')
		encoded = append(encoded, json...)
	}
	if len(encoded) > limit {
		return nil, errTooLarge
	}
	return encoded, nil
}

// Unpack decodes a message produced by Pack, rejecting inputs whose compressed
// or uncompressed size exceeds limit.
func Unpack(encoded []byte, limit int) ([]byte, error) {
	if len(encoded) > limit {
		return nil, errTooLarge
	}
	switch {
	case len(encoded) >= 1 && encoded[0] == 'J':
		return bytes.Clone(encoded[1:]), nil
	case len(encoded) >= 5 && encoded[0] == 'Z':
		size := uint64(binary.LittleEndian.Uint32(encoded[1:5]))
		if size > uint64(limit) {
			return nil, errDecodedTooLarge
		}
		return inflate(encoded[5:], int(size))
	}
	return nil, errEncoding
}

// inflate decompresses a single zlib stream and verifies it produces exactly
// size bytes with no trailing data. Because bytes.Reader implements
// flate.Reader, zlib won't over-read past the Adler-32 checksum.
func inflate(stream []byte, size int) ([]byte, error) {
	input := bytes.NewReader(stream)
	reader, err := zlib.NewReader(input)
	if err != nil {
		return nil, inflateError(err)
	}
	// Read up to size+1 bytes so an understated size header is caught immediately.
	decoded := make([]byte, 0, size+1)
	limited := io.LimitReader(reader, int64(size)+1)
	for {
		if len(decoded) == cap(decoded) {
			break
		}
		n, err := limited.Read(decoded[len(decoded):cap(decoded)])
		decoded = decoded[:len(decoded)+n]
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, inflateError(err)
		}
	}
	if len(decoded) != size || input.Len() != 0 {
		return nil, errInvalidZ
	}
	// Reaching EOF before the limit means zlib verified the checksum.
	return decoded, nil
}

func inflateError(err error) error {
	var corrupt flate.CorruptInputError
	switch {
	case errors.Is(err, io.EOF), errors.Is(err, io.ErrUnexpectedEOF):
		return errInvalidZ
	case errors.As(err, &corrupt), errors.Is(err, zlib.ErrChecksum),
		errors.Is(err, zlib.ErrHeader), errors.Is(err, zlib.ErrDictionary):
		return errInflate
	}
	return err
}

// Assembly reassembles strictly ordered fragments into a complete message. Any
// gap, duplicate, request ID change, or mismatched total resets the buffer so
// partial messages are never returned. The zero value is ready to use.
type Assembly struct {
	bytes   []byte
	total   int
	request uint32
}

var errFragments = errors.New("Invalid USB fragment sequence")

// Push appends a fragment and returns the completed message once the final
// fragment arrives (or nil while more fragments are pending).
func (a *Assembly) Push(frame *Frame, limit int) ([]byte, error) {
	end := frame.Offset + len(frame.Data)
	if frame.Total == 0 ||
		frame.Total > limit ||
		len(frame.Data) == 0 ||
		frame.Offset < 0 || end < frame.Offset || end > frame.Total ||
		(frame.Offset != 0 && (frame.Total != a.total || frame.Request != a.request)) ||
		frame.Offset != len(a.bytes) {
		a.Clear()
		return nil, errFragments
	}
	if frame.Offset == 0 {
		a.total = frame.Total
		a.request = frame.Request
	}
	a.bytes = append(a.bytes, frame.Data...)
	if len(a.bytes) == a.total {
		a.total = 0
		message := a.bytes
		a.bytes = nil
		return message, nil
	}
	return nil, nil
}

// Clear drops the in-flight message.
func (a *Assembly) Clear() {
	a.bytes = a.bytes[:0]
	a.total = 0
	a.request = 0
}
