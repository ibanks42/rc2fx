package control

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// mixer is a mixer for the client to talk to, with ways to lose reports.
type mixer struct {
	inbox        [][]byte
	assembly     usbproto.Assembly
	reply        []byte
	executed     int
	writes       *atomic.Int64
	short        bool
	loseFinalAck bool
	loseFetches  int
	// window is protocol 4's; 0 for a mixer that speaks only protocol 3.
	window int
	// loseReport drops this fetch reply report (by index), once.
	loseReport *int
	acks       *atomic.Int64
}

func newMixer() *mixer {
	return &mixer{writes: new(atomic.Int64), acks: new(atomic.Int64)}
}

func (m *mixer) answer(f *usbproto.Frame, kind usbproto.Kind, offset, total int, data []byte) {
	if kind == usbproto.KindAck {
		m.acks.Add(1)
	}
	// Real handles also see RØDE's reports and other sessions'.
	m.inbox = append(m.inbox, []byte{2, 'A'})
	for _, session := range []uint64{f.Session + 1, f.Session} {
		frame := usbproto.Frame{Kind: kind, Session: session, Request: f.Request, Offset: offset, Total: total, Data: data}
		encoded := frame.Encode(usbproto.InputID)
		m.inbox = append(m.inbox, bytes.Clone(encoded[:]))
	}
}

func (m *mixer) Write(report []byte, _ time.Duration) (int, error) {
	m.writes.Add(1)
	if m.short {
		return len(report) - 1, nil
	}
	f, ok := usbproto.DecodeFrame(report, usbproto.OutputID)
	if !ok {
		panic("undecodable host report")
	}
	switch f.Kind {
	case usbproto.KindHello:
		switch {
		case len(f.Data) == 0:
			m.answer(&f, usbproto.KindAck, 0, 0, []byte(usbproto.Hello3))
		case string(f.Data) == usbproto.Hello4 && m.window > 0:
			m.answer(&f, usbproto.KindAck, 0, 0, []byte(fmt.Sprintf("RC2FX USB 4 %d", m.window)))
		default:
			m.answer(&f, usbproto.KindError, 0, 0, []byte("Invalid USB hello"))
		}
	case usbproto.KindRequest:
		message, err := m.assembly.Push(&f, usbproto.MaxRequest)
		if err != nil {
			panic(err)
		}
		done := message != nil
		if done {
			m.executed++
		}
		end := (f.Offset + len(f.Data)) / usbproto.Payload
		windowed := m.window > 0 && !done && end%m.window != 0
		if !windowed && !(done && m.loseFinalAck) {
			m.answer(&f, usbproto.KindAck, f.Offset+len(f.Data), f.Total, nil)
		}
	case usbproto.KindFetch:
		switch {
		case m.executed == 0:
			m.answer(&f, usbproto.KindError, 0, 0, []byte("Invalid USB control packet"))
		case m.loseFetches > 0:
			m.loseFetches--
		default:
			offset := f.Offset
			for range max(f.Total, 1) {
				if offset >= len(m.reply) {
					break
				}
				end := min(offset+usbproto.Payload, len(m.reply))
				index := offset / usbproto.Payload
				if m.loseReport != nil && *m.loseReport == index {
					m.loseReport = nil
				} else {
					m.answer(&f, usbproto.KindReply, offset, len(m.reply), m.reply[offset:end])
				}
				offset = end
			}
		}
	default:
		panic("unexpected host operation")
	}
	return len(report), nil
}

func (m *mixer) Read(buf []byte, timeout time.Duration) (int, error) {
	if len(m.inbox) == 0 {
		time.Sleep(timeout)
		return 0, errors.New("timed out")
	}
	next := m.inbox[0]
	m.inbox = m.inbox[1:]
	return copy(buf, next), nil
}

func (m *mixer) Close() error { return nil }

func replyJSON(t *testing.T, body any) []byte {
	t.Helper()
	raw, err := sjson.Marshal(body)
	if err != nil {
		t.Fatal(err)
	}
	data, err := sjson.Marshal(usbproto.Reply{Status: 200, Body: json.RawMessage(raw)})
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func pack(t *testing.T, data []byte) []byte {
	t.Helper()
	packed, err := usbproto.Pack(data, usbproto.MaxReply)
	if err != nil {
		t.Fatal(err)
	}
	return packed
}

func client(t *testing.T, m *mixer) *Client {
	if m.reply == nil {
		m.reply = pack(t, replyJSON(t, map[string]any{"large": strings.Repeat("y", 4000)}))
	}
	model := firmware.ModelProIi
	return &Client{
		reports:     m,
		serial:      "GV1",
		model:       &model,
		nonce:       5,
		window:      1,
		reportWait:  20 * time.Millisecond,
		executeWait: 20 * time.Millisecond,
	}
}

// apply is a multi-report write, as an Apply sends.
func apply(t *testing.T) *Request {
	seed := uint32(1)
	var large strings.Builder
	for range 2000 {
		seed = seed*1664525 + 1013904223
		large.WriteByte(byte(' ' + (seed>>24)%90))
	}
	body, err := sjson.Marshal(map[string]any{"large": large.String()})
	if err != nil {
		t.Fatal(err)
	}
	return &Request{Method: "PUT", Path: "api/v1/config", Body: json.RawMessage(body)}
}

func body(t *testing.T, reply *Reply) map[string]any {
	t.Helper()
	value, err := sjson.Value(reply.Body)
	if err != nil {
		t.Fatal(err)
	}
	return value.(map[string]any)
}

func broken(t *testing.T, err error) {
	t.Helper()
	var failure *Failure
	if !errors.As(err, &failure) || failure.Kind != Broken {
		t.Fatalf("%v is not a broken session", err)
	}
}

func TestMultiReportRequestAndReplyIgnoreOtherReports(t *testing.T) {
	c := client(t, newMixer())
	reply, err := c.Request(apply(t))
	if err != nil {
		t.Fatal(err)
	}
	if reply.Status != 200 || len(body(t, reply)["large"].(string)) != 4000 || !c.IsOpen() {
		t.Fatalf("%+v", reply)
	}
}

func withReply(t *testing.T, data []byte) *Client {
	m := newMixer()
	m.reply = pack(t, data)
	return client(t, m)
}

func TestRepliesWithDuplicateKeysOrExcessiveDepthAreRefused(t *testing.T) {
	duplicate := []byte(`{"status":200,"body":{"a":1,"a":2}}`)
	deep := []byte(`{"status":200,"body":` + strings.Repeat("[", 65) + "1" + strings.Repeat("]", 65) + `}`)
	for _, data := range [][]byte{duplicate, deep} {
		_, err := withReply(t, data).Request(apply(t))
		broken(t, err)
	}
	reply, err := withReply(t, []byte(`{"status":200,"body":{"a":[1]}}`)).Request(apply(t))
	if err != nil {
		t.Fatal(err)
	}
	if fmt.Sprint(body(t, reply)["a"].([]any)[0]) != "1" {
		t.Fatalf("%s", reply.Body)
	}
}

func TestAnEventReplyIsHeldToTheEventLimit(t *testing.T) {
	events := &Request{Method: "GET", Path: eventsPath}
	event := func(size int) []byte {
		return replyJSON(t, map[string]any{"event": "meter", "data": strings.Repeat("y", size)})
	}
	reply, err := withReply(t, event(usbproto.MaxEventBytes)).Request(events)
	if err != nil || reply.Status != 200 {
		t.Fatalf("%v", err)
	}
	_, err = withReply(t, event(usbproto.MaxEventReply)).Request(events)
	broken(t, err)
	// Other requests keep the full reply limit.
	reply, err = withReply(t, event(usbproto.MaxEventReply)).Request(apply(t))
	if err != nil || reply.Status != 200 {
		t.Fatalf("%v", err)
	}
}

func TestAShortWriteBreaksTheSessionAndIsNeverReplayed(t *testing.T) {
	m := newMixer()
	m.short = true
	c := client(t, m)
	_, err := c.Request(apply(t))
	broken(t, err)
	if !strings.Contains(err.Error(), "not replayed") {
		t.Fatal(err)
	}
	if m.writes.Load() != 1 || c.IsOpen() {
		t.Fatal("replayed or still open")
	}
	_, err = c.Request(apply(t))
	var failure *Failure
	if !errors.As(err, &failure) || failure.Kind != NotSent {
		t.Fatalf("%v", err)
	}
}

func TestALostFinalAcknowledgementRecoversTheCachedReply(t *testing.T) {
	m := newMixer()
	m.loseFinalAck = true
	reply, err := client(t, m).Request(apply(t))
	if err != nil || reply.Status != 200 {
		t.Fatalf("%v", err)
	}
}

func TestLostRepliesAreFetchedAgain(t *testing.T) {
	m := newMixer()
	m.loseFetches = 2
	reply, err := client(t, m).Request(apply(t))
	if err != nil || reply.Status != 200 {
		t.Fatalf("%v", err)
	}
}

func TestOnlyTheSingleMixerControlInterfaceIsChosen(t *testing.T) {
	hid := func(pid uint16, serial string) platform.HID {
		return platform.HID{Serial: serial, PID: pid, Usage: 0xff00, Input: usbproto.ReportSize, Output: usbproto.ReportSize, Control: true, Release: 0x0177}
	}
	if h, err := controlInterface([]platform.HID{hid(0x0037, "A")}); err != nil || h.Serial != "A" {
		t.Fatal(err)
	}
	if _, err := controlInterface([]platform.HID{hid(0x0030, "A")}); err == nil {
		t.Fatal("update mode")
	}
	if _, err := controlInterface([]platform.HID{hid(0x0037, "A"), hid(0x0078, "B")}); err == nil {
		t.Fatal("two mixers")
	}
	if _, err := controlInterface(nil); err == nil {
		t.Fatal("none")
	}
	for _, pid := range firmware.ModelDuo.NormalPIDs() {
		if h, err := controlInterface([]platform.HID{hid(pid, "DUO")}); err != nil || h.Serial != "DUO" {
			t.Fatal(err)
		}
		stock := hid(pid, "DUO")
		stock.Control = false
		if _, err := controlInterface([]platform.HID{stock}); err == nil {
			t.Fatal("stock Duo")
		}
		if _, err := controlInterface([]platform.HID{hid(0x0037, "A"), hid(pid, "DUO")}); err == nil {
			t.Fatal("two models")
		}
	}
	stock := hid(0x0094, "A")
	stock.Control = false
	if _, err := controlInterface([]platform.HID{stock}); err == nil || !strings.Contains(err.Error(), "isn't running") {
		t.Fatal(err)
	}
	otherPage := hid(0x0094, "A")
	otherPage.Usage = 0xff52
	if _, err := controlInterface([]platform.HID{otherPage}); err == nil {
		t.Fatal("another usage page")
	}
}

// windowed is a protocol 4 client, negotiated with a mixer whose window is
// window.
func windowed(t *testing.T, window int, m *mixer) *Client {
	m.window = window
	c := client(t, m)
	if err := c.negotiate(); err != nil {
		t.Fatal(err)
	}
	return c
}

func TestProtocol4IsNegotiatedAnd3IsTheFallback(t *testing.T) {
	if v4 := windowed(t, 8, newMixer()); v4.window != 8 {
		t.Fatal(v4.window)
	}
	v3 := client(t, newMixer())
	if err := v3.negotiate(); err != nil || v3.window != 1 {
		t.Fatalf("%v %d", err, v3.window)
	}
}

func TestProtocol4AcknowledgesOncePerWindow(t *testing.T) {
	request := apply(t)
	acks := new(atomic.Int64)
	m := newMixer()
	m.acks = acks
	if reply, err := client(t, m).Request(request); err != nil || reply.Status != 200 {
		t.Fatal(err)
	}
	v3 := acks.Swap(0)
	m = newMixer()
	m.acks = acks
	c := windowed(t, 4, m)
	if hello := acks.Swap(0); hello != 1 {
		t.Fatalf("hello acks %d", hello)
	}
	if reply, err := c.Request(request); err != nil || reply.Status != 200 {
		t.Fatal(err)
	}
	v4 := acks.Load()
	if !(v3 > 4 && v4 == (v3+3)/4) {
		t.Fatalf("acks: v3 %d, v4 %d", v3, v4)
	}
}

func TestALostReplyReportIsFetchedAgainFromWhereItStopped(t *testing.T) {
	request := apply(t)
	data, err := sjson.Marshal(usbproto.Reply{Status: 200, Body: request.Body})
	if err != nil {
		t.Fatal(err)
	}
	reply := pack(t, data)
	if len(reply) <= 6*usbproto.Payload {
		t.Fatalf("reply of %d bytes is too short for this test", len(reply))
	}
	m := newMixer()
	m.reply = reply
	lose := 5
	m.loseReport = &lose
	got, err := windowed(t, 4, m).Request(apply(t))
	if err != nil {
		t.Fatal(err)
	}
	want, _ := sjson.Value(request.Body)
	have, _ := sjson.Value(got.Body)
	if fmt.Sprint(want) != fmt.Sprint(have) {
		t.Fatal("the reply body differs")
	}
}
