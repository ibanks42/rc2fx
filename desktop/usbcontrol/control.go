// Package usbcontrol is RC2FX control over USB 1: requests and replies framed
// into RC2FX's reports in the mixer's vendor HID collection (see
// docs/reference/usb-control.md). Blocking; one client owns the mixer's
// session at a time.
package usbcontrol

import (
	"bytes"
	"crypto/rand"
	"encoding/binary"
	"errors"
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
// rodecaster/platform so HID.Control reports whether RC2FX's USB layout is
// live on a mixer. Call before scanning HID devices when HID.Control is read.
func RegisterControlProbe() {
	platform.SetControlProbe(platform.ControlProbe{
		Descriptor: usbproto.DeclaresControl,
		OutputID:   usbproto.OutputID,
		InputID:    usbproto.InputID,
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

	timedOut = "timed out"
	notReady = "reply not ready"
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

// Client is a session with one mixer. Its nonce and request numbers last
// for the client's life, so reopening after a lost handle continues the
// same session on the mixer instead of waiting out another client's
// lockout.
type Client struct {
	reports  platform.Reports
	serial   string
	model    *firmware.Model
	nonce    uint64
	sequence uint32
	// window is the request fragments sent per acknowledgement, and reply
	// reports per fetch: 1 with a protocol 3 mixer, which acknowledges every
	// report.
	window      int
	reportWait  time.Duration
	executeWait time.Duration
}

// Open opens the one connected mixer's RC2FX control.
func Open() (*Client, error) {
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, err
	}
	c := &Client{
		nonce:       max(binary.LittleEndian.Uint64(nonce[:]), 1),
		window:      1,
		reportWait:  reportWait,
		executeWait: executeWait,
	}
	if err := c.connect(nil); err != nil {
		return nil, err
	}
	return c, nil
}

// Serial is the connected mixer's USB serial.
func (c *Client) Serial() string { return c.serial }

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
	return c.negotiate()
}

// negotiate tries protocol 4 first; a mixer that only speaks 3 refuses the
// marked hello without touching its session, and gets the plain one.
func (c *Client) negotiate() error {
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
func (c *Client) hello(data []byte) ([]byte, error) {
	frame := usbproto.Frame{Kind: usbproto.KindHello, Session: c.nonce, Data: data}
	reply, err := c.exchange(&frame, time.Now().Add(c.reportWait), c.reportWait)
	if err != nil {
		return nil, err
	}
	ack, ok := usbproto.DecodeFrame(reply[:], usbproto.InputID)
	if !ok || ack.Kind != usbproto.KindAck {
		return nil, textError("Invalid USB hello reply")
	}
	return bytes.Clone(ack.Data), nil
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
	packed, err := usbproto.Pack(json, usbproto.MaxRequest)
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
	count := (len(data) + usbproto.Payload - 1) / usbproto.Payload
	for index := range count {
		offset := index * usbproto.Payload
		chunk := data[offset:min(offset+usbproto.Payload, len(data))]
		last := index+1 == count
		frame := usbproto.Frame{
			Kind:    usbproto.KindRequest,
			Session: c.nonce,
			Request: c.sequence,
			Offset:  offset,
			Total:   len(data),
			Data:    chunk,
		}
		// Protocol 4 acknowledges only the fragment that ends a window (USB
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
		ack, ok := usbproto.DecodeFrame(reply[:], usbproto.InputID)
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
// asked for again rather than ending the request. A protocol 4 mixer answers
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
			reply, ok := usbproto.DecodeFrame(report[:], usbproto.InputID)
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
	encoded := frame.Encode(usbproto.OutputID)
	n, err := c.reports.Write(encoded[:], remaining)
	if err != nil {
		return err
	}
	if n < len(encoded) {
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
		if !usbproto.Marked(got, usbproto.InputID) {
			continue
		}
		reply, ok := usbproto.DecodeFrame(got, usbproto.InputID)
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
	// envelope's own decoder then applies serde's field rules.
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
