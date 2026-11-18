package transport

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer/control"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

const uploadSession = "0123456789abcdef"

// uploadMixer stands in for the mixer's update endpoints behind a USB
// session that breaks once, on the PUT numbered breakOn. A broken session
// stays closed until serve reopens it for a read, as the real client does.
type uploadMixer struct {
	open bool
	// The PUT (1-based) whose USB exchange breaks; 0 for none.
	breakOn int
	// Whether the mixer stored the broken PUT's chunk (only its reply was
	// lost) rather than never receiving it.
	storeBroken bool
	// Whether reopening fails, as with a mixer that stays unreachable.
	unreachable bool
	size        int
	data        []byte
	puts        int
	reopens     int
	installed   bool
}

func (m *uploadMixer) IsOpen() bool {
	return m.open
}

func (m *uploadMixer) Reopen() error {
	m.reopens++
	if m.unreachable {
		return errors.New("RC2FX did not answer over USB")
	}
	m.open = true
	return nil
}

func uploadReply(status uint16, fields ...member) (usbproto.Reply, error) {
	return usbproto.Reply{Status: status, Body: object(fields...)}, nil
}

func (m *uploadMixer) state() string {
	switch {
	case m.installed:
		return "restarting"
	case len(m.data) == m.size:
		return "complete"
	default:
		return "accepting"
	}
}

func (m *uploadMixer) Request(request *usbproto.Request) (usbproto.Reply, error) {
	path := "api/v1/update/" + uploadSession
	switch {
	case request.Method == "GET" && request.Path == "api/v1/update/installed":
		return uploadReply(404, member{"error", jsonString("not available")})
	case request.Method == "POST" && request.Path == "api/v1/update":
		return uploadReply(201,
			member{"id", jsonString(uploadSession)},
			member{"state", jsonString("accepting")},
			member{"received", jsonUint(0)},
		)
	case request.Method == "PUT" && request.Path == path:
		m.puts++
		var body struct {
			Offset int    `json:"offset"`
			Data   string `json:"data"`
		}
		if err := json.Unmarshal(request.Body, &body); err != nil {
			return uploadReply(400, member{"error", jsonString(err.Error())})
		}
		chunk, err := base64.StdEncoding.DecodeString(body.Data)
		if err != nil {
			return uploadReply(400, member{"error", jsonString(err.Error())})
		}
		breaks := m.puts == m.breakOn
		if !breaks || m.storeBroken {
			switch {
			case body.Offset == len(m.data) && len(m.data)+len(chunk) <= m.size:
				m.data = append(m.data, chunk...)
			// A chunk the mixer already holds is accepted again.
			case body.Offset+len(chunk) == len(m.data) && bytes.Equal(m.data[body.Offset:], chunk):
			default:
				return uploadReply(409,
					member{"error", jsonString("The upload does not continue there")},
					member{"expectedOffset", jsonUint(uint64(len(m.data)))},
				)
			}
		}
		if breaks {
			m.open = false
			return usbproto.Reply{}, &control.Failure{Kind: control.Broken, Message: "USB read timed out"}
		}
		return uploadReply(200, member{"received", jsonUint(uint64(len(m.data)))})
	case request.Method == "GET" && request.Path == path:
		return uploadReply(200,
			member{"id", jsonString(uploadSession)},
			member{"state", jsonString(m.state())},
			member{"received", jsonUint(uint64(len(m.data)))},
		)
	case request.Method == "POST" && request.Path == path+"/install":
		if len(m.data) != m.size {
			return uploadReply(409, member{"error", jsonString("The upload is incomplete")})
		}
		m.installed = true
		return uploadReply(200, member{"state", jsonString("installing")})
	default:
		return uploadReply(404, member{"error", jsonString("unknown")})
	}
}

// shortUploadRetries makes resuming quick for a test.
func shortUploadRetries(t *testing.T, window time.Duration) {
	t.Helper()
	savedWindow, savedFirst, savedMax := uploadRetryWindow, uploadRetryFirst, uploadRetryMax
	uploadRetryWindow, uploadRetryFirst, uploadRetryMax = window, 5*time.Millisecond, 20*time.Millisecond
	t.Cleanup(func() {
		uploadRetryWindow, uploadRetryFirst, uploadRetryMax = savedWindow, savedFirst, savedMax
	})
}

// uploadWith runs mixerUpdate for payload against session and returns its
// failure once the session's goroutine has stopped, so the caller may read
// session's fields.
func uploadWith(t *testing.T, session Session, payload []byte) *requestError {
	t.Helper()
	file := filepath.Join(t.TempDir(), "rc2fx.envelope")
	if err := os.WriteFile(file, payload, 0o600); err != nil {
		t.Fatal(err)
	}
	link := &usbLink{
		jobs:    make(chan job, 8),
		stopped: make(chan struct{}),
		holders: 1,
	}
	go func() {
		defer close(link.stopped)
		serve(link.jobs, session)
	}()
	d := device{Usb{link}}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	start := object(member{"version", jsonString("0.2.0")})
	failed := d.mixerUpdate(ctx, start, file, nil, func(sent, total uint64) {})
	d.release()
	<-link.stopped
	return failed
}

func uploadPayload() []byte {
	payload := make([]byte, 3*updateChunk+1000)
	for i := range payload {
		payload[i] = byte(i * 7)
	}
	return payload
}

func TestAnUploadResumesAfterTheUSBSessionBreaksMidUpload(t *testing.T) {
	shortUploadRetries(t, 5*time.Second)
	payload := uploadPayload()
	// The second chunk never reached the mixer.
	mixer := &uploadMixer{open: true, breakOn: 2, size: len(payload)}
	if failed := uploadWith(t, mixer, payload); failed != nil {
		t.Fatalf("the upload failed: %s", failed.message())
	}
	if !bytes.Equal(mixer.data, payload) {
		t.Fatalf("the mixer holds %d bytes that differ from the payload", len(mixer.data))
	}
	if !mixer.installed {
		t.Fatal("the upload was not installed")
	}
	if mixer.reopens != 1 {
		t.Fatalf("reopened %d times", mixer.reopens)
	}
	// Four chunks plus the one that was lost.
	if mixer.puts != 5 {
		t.Fatalf("sent %d chunks", mixer.puts)
	}
}

func TestAnUploadWhoseLastReplyWasLostInstallsWithoutResending(t *testing.T) {
	shortUploadRetries(t, 5*time.Second)
	payload := uploadPayload()
	// The last chunk arrived, but its reply was lost: the session is then
	// complete and nothing is left to send.
	mixer := &uploadMixer{open: true, breakOn: 4, storeBroken: true, size: len(payload)}
	if failed := uploadWith(t, mixer, payload); failed != nil {
		t.Fatalf("the upload failed: %s", failed.message())
	}
	if !bytes.Equal(mixer.data, payload) || !mixer.installed {
		t.Fatalf("installed %v with %d bytes", mixer.installed, len(mixer.data))
	}
	if mixer.puts != 4 {
		t.Fatalf("sent %d chunks", mixer.puts)
	}
}

func TestAnUploadGivesUpWhenTheMixerStaysUnreachable(t *testing.T) {
	window := 200 * time.Millisecond
	shortUploadRetries(t, window)
	payload := uploadPayload()
	mixer := &uploadMixer{open: true, breakOn: 2, unreachable: true, size: len(payload)}
	began := time.Now()
	failed := uploadWith(t, mixer, payload)
	if failed == nil || failed.kind != requestUSB {
		t.Fatalf("want a USB failure, got %v", failed)
	}
	if took := time.Since(began); took > window+2*time.Second {
		t.Fatalf("gave up after %s", took)
	}
	if mixer.reopens < 2 {
		t.Fatalf("tried to reopen %d times", mixer.reopens)
	}
	if mixer.installed {
		t.Fatal("an incomplete upload was installed")
	}
}
