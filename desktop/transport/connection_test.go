package transport

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// gatedMixer is a mixer whose first upload chunk is held until gate closes,
// so a test can act while an in-place update is mid-upload. It answers 404
// to everything else the app asks, the event stream included.
type gatedMixer struct {
	gate chan struct{}
	// Closed once the first chunk arrives.
	uploading chan struct{}

	mu       sync.Mutex
	puts     int
	received int
	// requests counts what was asked after the first chunk was answered.
	later []string
}

func newGatedMixer() *gatedMixer {
	return &gatedMixer{gate: make(chan struct{}), uploading: make(chan struct{})}
}

func (m *gatedMixer) IsOpen() bool  { return true }
func (m *gatedMixer) Reopen() error { return nil }

func (m *gatedMixer) Request(request *usbproto.Request) (usbproto.Reply, error) {
	path := "api/v1/update/" + uploadSession
	m.mu.Lock()
	if m.puts > 0 && request.Path != "api/v1/events" {
		m.later = append(m.later, request.Method+" "+request.Path)
	}
	m.mu.Unlock()
	switch {
	case request.Method == "POST" && request.Path == "api/v1/update":
		m.mu.Lock()
		m.received = 0
		m.mu.Unlock()
		return uploadReply(201,
			member{"id", jsonString(uploadSession)},
			member{"state", jsonString("accepting")},
			member{"received", jsonUint(0)},
		)
	case request.Method == "PUT" && request.Path == path:
		var body struct {
			Data string `json:"data"`
		}
		if err := json.Unmarshal(request.Body, &body); err != nil {
			return uploadReply(400, member{"error", jsonString(err.Error())})
		}
		chunk, err := base64.StdEncoding.DecodeString(body.Data)
		if err != nil {
			return uploadReply(400, member{"error", jsonString(err.Error())})
		}
		m.mu.Lock()
		m.puts++
		first := m.puts == 1
		m.mu.Unlock()
		if first {
			close(m.uploading)
			<-m.gate
		}
		m.mu.Lock()
		m.received += len(chunk)
		received := m.received
		m.mu.Unlock()
		return uploadReply(200, member{"received", jsonUint(uint64(received))})
	case request.Method == "GET" && request.Path == path:
		m.mu.Lock()
		received := m.received
		m.mu.Unlock()
		return uploadReply(200,
			member{"id", jsonString(uploadSession)},
			member{"state", jsonString("restarting")},
			member{"received", jsonUint(uint64(received))},
		)
	case request.Method == "POST" && request.Path == path+"/install":
		return uploadReply(200, member{"state", jsonString("installing")})
	default:
		return uploadReply(404, member{"error", jsonString("unknown")})
	}
}

func (m *gatedMixer) asked() (puts int, later []string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.puts, append([]string(nil), m.later...)
}

// fakeMixers connects to each of its sessions in turn, as connectDevice
// connects to a mixer, and keeps each session's link so a test can see when
// its goroutine stopped.
type fakeMixers struct {
	sessions []Session
	mu       sync.Mutex
	links    []*usbLink
}

func (f *fakeMixers) connect(context.Context) (device, string, json.RawMessage, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	session := f.sessions[len(f.links)]
	link := &usbLink{
		jobs:    make(chan job, 8),
		stopped: make(chan struct{}),
		holders: 1,
	}
	go func() {
		defer close(link.stopped)
		serve(link.jobs, session)
	}()
	f.links = append(f.links, link)
	return device{Usb{link}}, "GV1", json.RawMessage("{}"), nil
}

func (f *fakeMixers) link(i int) *usbLink {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.links[i]
}

// startRun runs the command loop against mixers until the test ends.
func startRun(t *testing.T, mixers *fakeMixers) (chan<- Command, <-chan Event) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	commands := make(chan Command, 16)
	events := make(chan Event, 64)
	done := make(chan struct{})
	go func() {
		defer close(done)
		run(ctx, commands, events, mixers.connect)
	}()
	t.Cleanup(func() {
		cancel()
		<-done
	})
	return commands, events
}

// await returns the first event of generation that want accepts, skipping
// the rest.
func await(t *testing.T, events <-chan Event, generation uint64, what string, want func(EventKind) bool) EventKind {
	t.Helper()
	timeout := time.After(5 * time.Second)
	for {
		select {
		case event := <-events:
			if event.Generation == generation && want(event.Kind) {
				return event.Kind
			}
		case <-timeout:
			t.Fatalf("no %s for generation %d", what, generation)
		}
	}
}

func isConnected(kind EventKind) bool {
	_, ok := kind.(EventKindConnected)
	return ok
}

func updateEvent(kind EventKind) (MixerUpdateEvent, bool) {
	update, ok := kind.(EventKindMixerUpdate)
	if !ok {
		return nil, false
	}
	return update.Event, true
}

func isUpdateFailed(kind EventKind) bool {
	event, ok := updateEvent(kind)
	_, failed := event.(MixerUpdateEventFailed)
	return ok && failed
}

func isUpdateInstalling(kind EventKind) bool {
	event, ok := updateEvent(kind)
	_, installing := event.(MixerUpdateEventInstalling)
	return ok && installing
}

func updateCommand(t *testing.T, generation uint64) CommandMixerUpdate {
	t.Helper()
	file := filepath.Join(t.TempDir(), "rc2fx.envelope")
	if err := os.WriteFile(file, uploadPayload(), 0o600); err != nil {
		t.Fatal(err)
	}
	return CommandMixerUpdate{
		Generation: generation,
		Start:      object(member{"version", jsonString("0.2.0")}),
		Payload:    file,
	}
}

func stopsWithin(t *testing.T, link *usbLink, what string) {
	t.Helper()
	select {
	case <-link.stopped:
	case <-time.After(5 * time.Second):
		t.Fatalf("%s still holds its USB session", what)
	}
}

func TestANewConnectionEndsTheUploadOfTheOldOne(t *testing.T) {
	old := newGatedMixer()
	mixers := &fakeMixers{sessions: []Session{old, newGatedMixer()}}
	commands, events := startRun(t, mixers)
	commands <- CommandConnect{Generation: 1}
	await(t, events, 1, "connection", isConnected)
	commands <- updateCommand(t, 1)
	<-old.uploading
	// The first chunk is still with the old mixer when the app connects
	// again. The new connection does not wait for that request.
	commands <- CommandConnect{Generation: 2}
	await(t, events, 2, "second connection", isConnected)
	close(old.gate)
	// With the upload gone, nothing holds the old session, and nothing more
	// was sent on it.
	stopsWithin(t, mixers.link(0), "the old connection")
	if puts, later := old.asked(); puts != 1 || len(later) != 0 {
		t.Fatalf("the old session got %d chunks and then %v", puts, later)
	}
}

func TestDisconnectingEndsAnUploadInFlight(t *testing.T) {
	mixer := newGatedMixer()
	mixers := &fakeMixers{sessions: []Session{mixer}}
	commands, events := startRun(t, mixers)
	commands <- CommandConnect{Generation: 1}
	await(t, events, 1, "connection", isConnected)
	commands <- updateCommand(t, 1)
	<-mixer.uploading
	commands <- CommandDisconnect{Generation: 2}
	// Commands run in order: once an update for generation 2 is refused for
	// having no mixer, the disconnect has finished dropping the old
	// connection, so opening the gate can no longer race it.
	commands <- updateCommand(t, 2)
	refused := await(t, events, 2, "refusal without a mixer", isUpdateFailed)
	if failed, _ := updateEvent(refused); !strings.Contains(failed.(MixerUpdateEventFailed).Message, "No connected mixer") {
		t.Fatalf("refused with %#v", failed)
	}
	close(mixer.gate)
	stopsWithin(t, mixers.link(0), "the dropped connection")
	if puts, later := mixer.asked(); puts != 1 || len(later) != 0 {
		t.Fatalf("the session got %d chunks and then %v", puts, later)
	}
}

func TestASecondUpdateIsRefusedWhileOneIsSending(t *testing.T) {
	mixer := newGatedMixer()
	mixers := &fakeMixers{sessions: []Session{mixer}}
	commands, events := startRun(t, mixers)
	commands <- CommandConnect{Generation: 1}
	await(t, events, 1, "connection", isConnected)
	commands <- updateCommand(t, 1)
	<-mixer.uploading
	commands <- updateCommand(t, 1)
	refused := await(t, events, 1, "refusal", isUpdateFailed)
	failed, _ := updateEvent(refused)
	if message := failed.(MixerUpdateEventFailed).Message; !strings.Contains(message, "still being sent") {
		t.Fatalf("refused with %q", message)
	}
	// The first update goes on and installs.
	close(mixer.gate)
	await(t, events, 1, "install", isUpdateInstalling)
	if _, later := mixer.asked(); len(later) == 0 || later[len(later)-1] != "GET api/v1/update/"+uploadSession {
		t.Fatalf("after the first chunk the mixer was asked %v", later)
	}
	// Once it ended, another update may start.
	commands <- updateCommand(t, 1)
	if kind := await(t, events, 1, "second update", func(kind EventKind) bool {
		_, ok := updateEvent(kind)
		return ok
	}); isUpdateFailed(kind) {
		t.Fatalf("a later update was refused: %#v", kind)
	}
}
