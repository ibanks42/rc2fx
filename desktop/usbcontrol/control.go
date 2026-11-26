// Package usbcontrol implements RC2FX control over USB 1 by framing requests
// and replies into vendor HID reports (see docs/reference/usb-control.md).
// Calls are blocking and one client owns the mixer session at a time.
package usbcontrol

import (
	"bytes"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"math"
	"runtime"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// RegisterControlProbe registers RC2FX's USB control report probe with
// rodecaster/platform so HID.Control and HID.CompactControl report whether
// RC2FX's USB layout is live on a mixer. Call before scanning HID devices when
// HID.Control is read.
func RegisterControlProbe() {
	platform.SetControlProbe(platform.ControlProbe{
		Descriptor:      usbproto.DetectControl,
		OutputID:        usbproto.OutputID,
		InputID:         usbproto.InputID,
		CompactOutputID: usbproto.CompactOutputID,
		CompactInputID:  usbproto.CompactInputID,
	})
}

// Request and Reply are the control protocol's messages.
type (
	Request = usbproto.Request
	Reply   = usbproto.Reply
)

const (
	// deadline is the longest a request may take, including the mixer
	// executing it.
	deadline = 60 * time.Second
	// reportWait: the mixer answers a report at once; longer silence means it
	// was lost.
	reportWait = 3 * time.Second
	// executeWait: a request executes before its final acknowledgement, for
	// up to 15 s.
	executeWait = 17 * time.Second
	// fetchWindow is the most reply reports asked for per fetch: macOS queues
	// about 30 input reports and Linux's hidraw 64, shared with RØDE's own.
	fetchWindow = 16
	// bytesPerNode is the mixer's own bound on a saved file's density,
	// applied to a reply's size: real replies stay far below it, and a parsed
	// value costs many times its text.
	bytesPerNode = 4
	// eventsPath: the event poll runs twenty times a second and is never a
	// whole settings file.
	eventsPath = "api/v1/events"
	// padShortWrites: Windows' HID WriteFile takes buffers sized to the
	// collection's longest output report (256 bytes); hidclass.sys looks up
	// report 64's declared 62-byte size and sends a single 62-byte packet on
	// the wire. Linux and macOS take the 62-byte report directly.
	padShortWrites = runtime.GOOS == "windows"
	timedOut       = "timed out"
	notReady       = "reply not ready"
)

// FailureKind says whether a failed request reached the mixer.
type FailureKind int

const (
	// NotSent: nothing reached the mixer; the session is intact.
	NotSent FailureKind = iota
	// Broken: the session is broken and the request's outcome may be unknown.
	// It is never replayed; reopen before sending more.
	Broken
)

// Failure is why a request failed.
type Failure struct {
	Kind    FailureKind
	Message string
}

func (f *Failure) Error() string { return f.Message }

type textError string

func (e textError) Error() string { return string(e) }

// Client is a session with one mixer. Its nonce and request numbers last for
// the client's life, and can be handed off across an app restart (see
// handoff.go), so reopening or restarting continues the same session instead
// of waiting out the mixer's lockout.
type Client struct {
	reports  platform.Reports
	serial   string
	model    *firmware.Model
	nonce    uint64
	sequence uint32
	// window is the request fragments sent per acknowledgement, and reply
	// reports per fetch: 1 with a protocol 3 mixer, which acknowledges every
	// report.
	window int
	// payload is the request bytes per fragment and inPayload the reply bytes
	// per input report: (Payload, Payload) on legacy reports 5/6, or
	// (CompactOutPayload, CompactInPayload) on compact reports 64/65.
	payload   int
	inPayload int
	// compact is true when talking protocol 5 on reports 64/65 (20-byte header,
	// 62-byte single-packet host writes, zstd compression).
	compact bool
	// shortWorks is set once a compact report 64 write succeeds; fullOnly once
	// the host API refuses the first report 64 write, so reconnects fall back
	// to legacy reports 5/6.
	shortWorks  bool
	fullOnly    bool
	reportWait  time.Duration
	executeWait time.Duration
	// resume continues a previous process's session if connect finds the same
	// mixer (see handoff.go).
	resume *sessionToken
}

// Open opens the one connected mixer's RC2FX control.
func Open() (*Client, error) {
	// Keep the random nonce within uint32 so the same session ID works for
	// both compact reports (32-bit session) and legacy reports (64-bit).
	var nonce [4]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, err
	}
	c := &Client{
		nonce:       uint64(max(binary.LittleEndian.Uint32(nonce[:]), 1)),
		window:      1,
		payload:     usbproto.Payload,
		inPayload:   usbproto.Payload,
		reportWait:  reportWait,
		executeWait: executeWait,
		resume:      takeResume(),
	}
	if err := c.connect(nil); err != nil {
		return nil, err
	}
	return c, nil
}

// Serial is the connected mixer's USB serial.
func (c *Client) Serial() string { return c.serial }

// IsCompact reports whether this session negotiated compact protocol 5 (reports 64/65).
func (c *Client) IsCompact() bool { return c.compact }

// IsOpen is whether the handle is open.
func (c *Client) IsOpen() bool { return c.reports != nil }

// Close closes the handle; queued reports for it are never read.
func (c *Client) Close() {
	if c.reports != nil {
		c.reports.Close()
		c.reports = nil
	}
}

// Reopen reopens after a broken request: the same mixer only.
func (c *Client) Reopen() error {
	serial := c.serial
	return c.connect(&serial)
}

func (c *Client) connect(expected *string) error {
	c.Close()
	RegisterControlProbe()
	host := platform.Host()
	items, err := host.Inventory()
	if err != nil {
		return err
	}
	hid, err := controlInterface(items)
	if err != nil {
		return err
	}
	model, ok := firmware.ModelFromUSB(firmware.VendorID, hid.PID)
	if c.model != nil && (!ok || *c.model != model) {
		return textError("A different mixer model is connected now")
	}
	if expected != nil && hid.Serial != *expected {
		return textError("A different RØDECaster Pro II is connected now")
	}
	// Windows reports descriptor lengths; elsewhere the inventory gives
	// buffer capacities and the HID API takes the 256-byte reports.
	if runtime.GOOS == "windows" && (hid.Input != usbproto.ReportSize || hid.Output != usbproto.ReportSize) {
		return textError("The mixer's HID report layout is not supported")
	}
	if r := c.resume; r != nil {
		c.resume = nil
		if r.serial == hid.Serial && (!hid.CompactControl || r.nonce <= math.MaxUint32) {
			c.nonce, c.sequence = r.nonce, r.sequence
		}
	}
	reports, err := host.OpenControl(hid)
	if err != nil {
		return err
	}
	c.reports = reports
	c.serial = hid.Serial
	if ok {
		c.model = &model
	} else {
		c.model = nil
	}
	if err := c.negotiate(hid.CompactControl); err != nil {
		return err
	}
	c.remember()
	return nil
}

// negotiate speaks protocol 5 on compact reports 64/65 when declared by the
// mixer's HID descriptor, falling back to legacy reports 5/6 (protocol 4,
// then 3) for 0.2.4 mixers or hosts that refuse report 64 writes.
func (c *Client) negotiate(hasCompact bool) error {
	c.compact = false
	c.payload, c.inPayload = usbproto.Payload, usbproto.Payload
	if hasCompact && !c.fullOnly {
		c.compact = true
		c.payload, c.inPayload = usbproto.CompactOutPayload, usbproto.CompactInPayload
		data, err := c.hello([]byte(usbproto.Hello5))
		switch {
		case err == nil:
			w, found := usbproto.Hello5Window(data)
			if !found {
				c.Close()
				return textError("RC2FX's USB protocol version does not match this app")
			}
			c.window = min(w, usbproto.MaxWindow)
			return nil
		case c.fullOnly, err.Error() == "Invalid USB hello":
			c.compact = false
			c.payload, c.inPayload = usbproto.Payload, usbproto.Payload
		default:
			return c.refused(err)
		}
	}
	window, ok := 0, false
	data, err := c.hello([]byte(usbproto.Hello4))
	switch {
	case err == nil:
		if w, found := usbproto.Hello4Window(data); found {
			window, ok = min(w, usbproto.MaxWindow), true
		}
	case err.Error() == "Invalid USB hello":
		data, err := c.hello(nil)
		if err != nil {
			return c.refused(err)
		}
		if bytes.Equal(data, []byte(usbproto.Hello3)) {
			window, ok = 1, true
		}
	default:
		return c.refused(err)
	}
	if !ok {
		c.Close()
		return textError("RC2FX's USB protocol version does not match this app")
	}
	c.window = window
	return nil
}

// hello says hello with data; it returns the mixer's acknowledgement text.
// If the first attempt times out reading a reply (for instance after a USB
// write timeout leaves host and device out of sync on DATA0/DATA1 toggle so
// the first packet is ACK-discarded to resync), retry the idempotent hello once.
func (c *Client) hello(data []byte) ([]byte, error) {
	frame := usbproto.Frame{Kind: usbproto.KindHello, Session: c.nonce, Data: data}
	firstWait := min(c.reportWait, 500*time.Millisecond)
	reply, err := c.exchange(&frame, time.Now().Add(firstWait), firstWait)
	if err != nil && err.Error() == timedOut {
		reply, err = c.exchange(&frame, time.Now().Add(c.reportWait), c.reportWait)
	}
	if err != nil {
		return nil, err
	}
	ack, ok := c.decodeInput(reply[:])
	if !ok || ack.Kind != usbproto.KindAck {
		return nil, textError("Invalid USB hello reply")
	}
	return bytes.Clone(ack.Data), nil
}

func (c *Client) decodeInput(bytes []byte) (usbproto.Frame, bool) {
	if c.compact {
		return usbproto.DecodeCompactFrame(bytes, usbproto.CompactInputID)
	}
	return usbproto.DecodeFrame(bytes, usbproto.InputID)
}

func (c *Client) markedInput(bytes []byte) bool {
	if c.compact {
		return usbproto.MarkedCompact(bytes, usbproto.CompactInputID)
	}
	return usbproto.Marked(bytes, usbproto.InputID)
}

func (c *Client) refused(err error) error {
	c.Close()
	if strings.HasPrefix(err.Error(), "USB is in use") {
		return textError(err.Error() + "; try again in a few seconds")
	}
	return textError("RC2FX did not answer over USB: " + err.Error())
}

// Request sends one request and waits for its reply.
func (c *Client) Request(request *Request) (*Reply, error) {
	json, err := sjson.Marshal(request)
	if err != nil {
		return nil, &Failure{NotSent, err.Error()}
	}
	var packed []byte
	if c.compact {
		packed, err = usbproto.PackZstd(json, usbproto.MaxRequest)
	} else {
		packed, err = usbproto.Pack(json, usbproto.MaxRequest)
	}
	if err != nil {
		return nil, &Failure{NotSent, err.Error()}
	}
	if c.reports == nil {
		return nil, &Failure{NotSent, "USB is not connected"}
	}
	if c.sequence == ^uint32(0) {
		return nil, &Failure{NotSent, "USB session exhausted; reconnect"}
	}
	c.sequence++
	c.remember()
	limit := usbproto.MaxReply
	if request.Path == eventsPath {
		limit = usbproto.MaxEventReply
	}
	err = c.send(packed)
	var reply *Reply
	if err == nil {
		reply, err = c.fetch(limit)
	}
	if err != nil {
		c.Close()
		return nil, &Failure{Broken, err.Error()}
	}
	return reply, nil
}

func (c *Client) send(data []byte) error {
	end := time.Now().Add(deadline)
	count := (len(data) + c.payload - 1) / c.payload
	for index := range count {
		offset := index * c.payload
		chunk := data[offset:min(offset+c.payload, len(data))]
		last := index+1 == count
		frame := usbproto.Frame{
			Kind:    usbproto.KindRequest,
			Session: c.nonce,
			Request: c.sequence,
			Offset:  offset,
			Total:   len(data),
			Data:    chunk,
		}
		// Protocol 4/5 acknowledges only the fragment that ends a window (USB
		// holds the next while the mixer is busy); the rest are only written.
		if !last && (index+1)%c.window != 0 {
			if err := c.write(&frame, earliest(end, time.Now().Add(c.reportWait))); err != nil {
				return err
			}
			continue
		}
		wait := c.reportWait
		if last {
			wait = c.executeWait
		}
		reply, err := c.exchange(&frame, end, wait)
		if err != nil {
			// The last fragment's acknowledgement can be lost after the mixer
			// executed it; its reply is still cached for fetching.
			if last && err.Error() == timedOut {
				return nil
			}
			return err
		}
		ack, ok := c.decodeInput(reply[:])
		if !ok {
			return textError("Invalid USB acknowledgement")
		}
		if ack.Kind != usbproto.KindAck || ack.Offset != offset+len(chunk) || ack.Total != len(data) || len(ack.Data) != 0 {
			return textError("USB fragment was not acknowledged; not replayed")
		}
	}
	return nil
}

func earliest(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}

// fetch fetches the reply. Fetching is idempotent, so a lost report is
// asked for again rather than ending the request. A protocol 4/5 mixer answers
// one fetch with several consecutive reports.
func (c *Client) fetch(limit int) (*Reply, error) {
	end := time.Now().Add(deadline)
	var assembly usbproto.Assembly
	offset := 0
	count := 0
	if c.window > 1 {
		count = min(c.window, fetchWindow)
	}
	for {
		frame := usbproto.Frame{
			Kind:    usbproto.KindFetch,
			Session: c.nonce,
			Request: c.sequence,
			Offset:  offset,
			Total:   count,
		}
		until := earliest(end, time.Now().Add(c.reportWait))
		first, firstErr := c.exchange(&frame, end, c.reportWait)
		haveFirst := true
		for range max(count, 1) {
			var report [usbproto.ReportSize]byte
			var err error
			if haveFirst {
				report, err, haveFirst = first, firstErr, false
			} else {
				report, err = c.read(&frame, until)
			}
			if err != nil {
				text := err.Error()
				now := time.Now()
				if text == timedOut && now.Before(end) {
					break
				}
				// Before the mixer has a reply (still executing, or its final
				// acknowledgement was lost) a fetch is refused.
				if text == notReady && now.Before(end) {
					time.Sleep(100 * time.Millisecond)
					break
				}
				if text == timedOut || text == notReady {
					return nil, textError("USB request timed out; its outcome is unknown; not replayed")
				}
				return nil, err
			}
			reply, ok := c.decodeInput(report[:])
			if !ok {
				return nil, textError("Invalid USB reply")
			}
			if reply.Kind != usbproto.KindReply {
				return nil, textError("Invalid USB reply sequence")
			}
			// A report from an earlier fetch of this request: the reply is
			// cached, so an offset always carries the same bytes.
			if reply.Offset < offset {
				continue
			}
			// One went missing; ask again from where the reply stopped.
			if reply.Offset > offset {
				break
			}
			offset += len(reply.Data)
			data, err := assembly.Push(&reply, limit)
			if err != nil {
				return nil, err
			}
			if data != nil {
				unpacked, err := usbproto.Unpack(data, limit)
				if err != nil {
					return nil, err
				}
				return parseReply(unpacked, limit)
			}
		}
		if !time.Now().Before(end) {
			return nil, textError("USB request timed out; its outcome is unknown; not replayed")
		}
	}
}

// exchange writes one report and reads this session's answer to it,
// waiting at most wait (and never past end).
func (c *Client) exchange(frame *usbproto.Frame, end time.Time, wait time.Duration) ([usbproto.ReportSize]byte, error) {
	until := earliest(end, time.Now().Add(wait))
	if err := c.write(frame, until); err != nil {
		return [usbproto.ReportSize]byte{}, err
	}
	return c.read(frame, until)
}

func (c *Client) write(frame *usbproto.Frame, until time.Time) error {
	remaining := time.Until(until)
	if remaining < 0 {
		return textError(timedOut)
	}
	if c.reports == nil {
		return textError("USB is not connected")
	}
	var report []byte
	if c.compact {
		short := frame.EncodeCompactOut()
		if padShortWrites {
			var padded [usbproto.ReportSize]byte
			copy(padded[:], short[:])
			report = padded[:]
		} else {
			report = short[:]
		}
	} else {
		full := frame.Encode(usbproto.OutputID)
		report = full[:]
	}
	n, err := c.reports.Write(report, remaining)
	if c.compact {
		if err == nil {
			c.shortWorks = true
		} else if !c.shortWorks {
			c.fullOnly = true
		}
	}
	if err != nil {
		return err
	}
	if n < len(report) {
		return textError("Short USB write; not replayed")
	}
	return nil
}

// read reads this session's next report for frame's request, until until.
func (c *Client) read(frame *usbproto.Frame, until time.Time) ([usbproto.ReportSize]byte, error) {
	var none [usbproto.ReportSize]byte
	if c.reports == nil {
		return none, textError("USB is not connected")
	}
	for {
		remaining := time.Until(until)
		if remaining < 0 {
			return none, textError(timedOut)
		}
		var report [usbproto.ReportSize]byte
		n, err := c.reports.Read(report[:], remaining)
		if err != nil {
			// A read that fails at the deadline is a timeout, whatever the
			// platform calls it.
			if time.Until(until) < 0 {
				return none, textError(timedOut)
			}
			return none, err
		}
		got := report[:min(n, usbproto.ReportSize)]
		// RØDE's reports and other sessions' may arrive on the same handle.
		if !c.markedInput(got) {
			continue
		}
		reply, ok := c.decodeInput(got)
		if !ok {
			return none, textError("Malformed USB reply")
		}
		if reply.Session != c.nonce || reply.Request != frame.Request {
			continue
		}
		if reply.Kind == usbproto.KindError {
			text := strings.ToValidUTF8(string(reply.Data), "�")
			if frame.Kind == usbproto.KindFetch && text == "Invalid USB control packet" {
				return none, textError(notReady)
			}
			return none, textError(text)
		}
		return report, nil
	}
}

// parseReply: a reply is the mixer's data, so it gets the same
// duplicate-key, depth and size checks as any other JSON RC2FX reads.
func parseReply(data []byte, limit int) (*Reply, error) {
	if !utf8.Valid(data) {
		return nil, errors.New("invalid utf-8 sequence")
	}
	// core's parser applies the limits and refuses duplicate keys; the
	// envelope's own decoder then applies strict field rules.
	if _, err := core.ParseJSONLimited(string(data), limit, limit/bytesPerNode); err != nil {
		return nil, err
	}
	var reply Reply
	if err := reply.UnmarshalJSON(data); err != nil {
		return nil, err
	}
	return &reply, nil
}

// controlInterface is the single mixer's RC2FX control interface: RØDE's
// vendor collection, once RC2FX's reports are declared in it.
func controlInterface(items []platform.HID) (*platform.HID, error) {
	var mixers []*platform.HID
	for i := range items {
		h := &items[i]
		if h.Usage != 0xff00 || h.Serial == "" {
			continue
		}
		model, ok := firmware.ModelFromUSB(firmware.VendorID, h.PID)
		if !ok {
			continue
		}
		normal := false
		for _, pid := range model.NormalPIDs() {
			if pid == h.PID {
				normal = true
			}
		}
		if normal {
			mixers = append(mixers, h)
		}
	}
	switch len(mixers) {
	case 0:
		return nil, textError("No RØDECaster Pro II connected")
	case 1:
		// The descriptor and handshake prove this is an intentionally
		// installed RC2FX runtime. The saved flash opt-in is not an uninstall
		// switch.
		if mixers[0].Control {
			return mixers[0], nil
		}
		return nil, textError("The mixer is connected, but RC2FX isn't running on it")
	}
	return nil, textError("More than one RØDECaster Pro II is connected; connect only one")
}
