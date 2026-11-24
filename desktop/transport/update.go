package transport

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/delta"
)

// updateChunk is the bytes per update chunk: base64 and the JSON around it
// stay well inside requestLimit, and the mixer accepts up to 256 KiB.
const updateChunk = 192 * 1024

// An upload whose USB session broke resumes after waiting uploadRetryFirst,
// doubled up to uploadRetryMax between attempts, and gives up once
// uploadRetryWindow passes without a chunk getting through. Variables so tests
// can shorten them.
var (
	uploadRetryWindow = 30 * time.Second
	uploadRetryFirst  = 250 * time.Millisecond
	uploadRetryMax    = 4 * time.Second
)

// deltaLevel is zstd's level for update deltas: about 3 s for a whole release
// here, and every byte saved is worth far more over USB.
const deltaLevel = 19

// baseFiles is a signed payload's files by name.
func baseFiles(envelope []byte) (map[string][]byte, error) {
	at, entries, err := delta.EnvelopeEntries(envelope)
	if err != nil {
		return nil, err
	}
	files := make(map[string][]byte, len(entries))
	for _, entry := range entries {
		size := entry.Size
		if at < 0 || size > uint64(len(envelope)) || uint64(at)+size > uint64(len(envelope)) {
			return nil, errors.New("the base payload is truncated")
		}
		end := at + int(size)
		files[entry.Name] = append([]byte(nil), envelope[at:end]...)
		at = end
	}
	return files, nil
}

// encodeBase64 is standard base64 with padding, as the mixer's update API
// reads it.
func encodeBase64(b []byte) string {
	return base64.StdEncoding.EncodeToString(b)
}

// delta computes a delta update against the mixer's installed files when the
// mixer supports `inPlaceUpdates` v2: unchanged files are copied on-device,
// changed files are patched against base (when available), and the rest are
// sent compressed. The mixer reconstructs and verifies the full signed release.
// Returns (nil, nil) if the mixer only accepts whole releases (404).
func (d device) delta(ctx context.Context, envelope []byte, base *string) ([]byte, *requestError) {
	value, failed := d.request(ctx, "GET", "api/v1/update/installed", nil)
	switch {
	case failed == nil:
	// A mixer that takes only whole releases.
	case failed.isStatus(404):
		return nil, nil
	default:
		return nil, failed
	}
	installed, ok := installedFiles(value)
	if !ok {
		return nil, errProtocol()
	}
	var baseEnvelope []byte
	if base != nil {
		if bytes, err := os.ReadFile(*base); err == nil {
			baseEnvelope = bytes
		}
	}
	type encoded struct {
		delta []byte
		err   error
	}
	result := make(chan encoded, 1)
	go func() {
		bases := map[string][]byte{}
		if baseEnvelope != nil {
			if files, err := baseFiles(baseEnvelope); err == nil {
				bases = files
			}
		}
		out, err := delta.Encode(envelope, installed, bases, deltaLevel)
		result <- encoded{out, err}
	}()
	select {
	case r := <-result:
		if r.err != nil {
			return nil, usbError(fmt.Sprintf("Cannot prepare the mixer software: %s", r.err))
		}
		return r.delta, nil
	case <-ctx.Done():
		return nil, usbError(ctx.Err().Error())
	}
}

// installedFiles reads `files` of an installed-files reply: each file the
// mixer has and its SHA-256.
func installedFiles(value json.RawMessage) (map[string]string, bool) {
	files, ok := field(value, "files")
	if !ok {
		return nil, false
	}
	entries, ok := members(files)
	if !ok {
		return nil, false
	}
	installed := make(map[string]string, len(entries))
	for name, raw := range entries {
		sha, ok := asString(raw)
		if !ok {
			return nil, false
		}
		installed[name] = sha
	}
	return installed, true
}

// mixerUpdate uploads payload for the in-place update start describes,
// resuming where the mixer says it is, then asks it to verify and install.
// Returns once the mixer has accepted the release; it then restarts by
// itself.
func (d device) mixerUpdate(ctx context.Context, start json.RawMessage, payload string, base *string, drive DriveWriter, copying func(total uint64), progress func(sent, total uint64)) *requestError {
	envelope, err := os.ReadFile(payload)
	if err != nil {
		return usbError(fmt.Sprintf("Cannot read the mixer software: %s", readError(err)))
	}
	if start == nil {
		start = json.RawMessage("null")
	}
	changes, failed := d.delta(ctx, envelope, base)
	if failed != nil {
		return failed
	}
	payloadBytes := envelope
	if changes != nil {
		edited, ok := withMembers(start,
			member{"encoding", jsonString("delta-1")},
			member{"uploadSize", jsonUint(uint64(len(changes)))},
		)
		if !ok {
			return errProtocol()
		}
		start = edited
		payloadBytes = changes
	}
	total := uint64(len(payloadBytes))
	session, failed := d.requestWithStatus(ctx, "POST", "api/v1/update", start, 201)
	if failed != nil {
		return failed
	}
	id, ok := stringField(session, "id")
	if !ok || len(id) != 16 || !common.IsHex(id) {
		return errProtocol()
	}
	path := "api/v1/update/" + id
	offset := uint64(0)
	// Set while resuming after a broken USB session; cleared by a chunk that
	// gets through.
	var retryUntil time.Time
	wait := uploadRetryFirst
	if drive != nil && d.throughDrive(ctx, path, payloadBytes, drive, copying) {
		offset = total
	}
	progress(offset, total)
	for offset < total {
		if ctx.Err() != nil {
			return usbError(ctx.Err().Error())
		}
		end := min(offset+updateChunk, total)
		body := object(
			member{"offset", jsonUint(offset)},
			member{"data", jsonString(encodeBase64(payloadBytes[offset:end]))},
		)
		reply, failed := d.usb.request(ctx, "PUT", path, body)
		switch {
		case failed == nil && reply.Status == 200:
			received, ok := uintField(reply.Body, "received")
			if !ok || received <= offset || received > total {
				return errProtocol()
			}
			offset = received
			retryUntil = time.Time{}
			wait = uploadRetryFirst
			progress(offset, total)
		// The mixer is elsewhere (a reply was lost): continue from there.
		case failed == nil && reply.Status == 409:
			expected, ok := uintField(reply.Body, "expectedOffset")
			if !ok || expected > total {
				return statusError(409, errorDetail(reply.Body))
			}
			offset = expected
		case failed == nil:
			return statusError(reply.Status, errorDetail(reply.Body))
		// The USB session broke, so the chunk may or may not have arrived,
		// and the session refuses every change until a read reopens it.
		case failed.kind == requestUSB:
			if ctx.Err() != nil {
				return usbError(ctx.Err().Error())
			}
			if retryUntil.IsZero() {
				retryUntil = time.Now().Add(uploadRetryWindow)
			}
			resumed, unresumed := d.resumeUpload(ctx, path, total, failed, retryUntil, &wait)
			if unresumed != nil {
				return unresumed
			}
			offset = resumed
			progress(offset, total)
		default:
			return failed
		}
	}
	if _, failed := d.request(ctx, "POST", path+"/install", nil); failed != nil {
		return failed
	}
	// The mixer checks the metadata before answering, then the payload on
	// its own thread: wait until it installs (and restarts) or refuses.
	deadline := time.Now().Add(180 * time.Second)
	for time.Now().Before(deadline) {
		if !sleep(ctx, time.Second) {
			return usbError(ctx.Err().Error())
		}
		session, failed := d.request(ctx, "GET", path, nil)
		switch {
		case failed == nil:
			state, ok := stringField(session, "state")
			switch {
			case !ok:
				return errProtocol()
			case state == "failed":
				return statusError(409, errorDetail(session))
			case state == "restarting":
				return nil
			}
		// The controller is being replaced: the install went ahead.
		case failed.kind == requestUSB || failed.isStatus(404):
			return nil
		default:
			return failed
		}
	}
	return nil
}

// resumeUpload finds where an upload continues after its USB session broke.
// Only a body-less read reopens the session, so it waits (with backoff in
// wait) and reads the update session at path until that read gets through,
// then answers the offset the mixer has reached: total once the upload is
// complete. cause is returned if the session is still unreachable at until.
func (d device) resumeUpload(ctx context.Context, path string, total uint64, cause *requestError, until time.Time, wait *time.Duration) (uint64, *requestError) {
	for time.Now().Add(*wait).Before(until) {
		if !sleep(ctx, *wait) {
			return 0, usbError(ctx.Err().Error())
		}
		*wait = min(*wait*2, uploadRetryMax)
		session, failed := d.request(ctx, "GET", path, nil)
		switch {
		case failed == nil:
			return resumePoint(session, total)
		case failed.kind == requestUSB:
			if ctx.Err() != nil {
				return 0, usbError(ctx.Err().Error())
			}
			cause = failed
		default:
			return 0, failed
		}
	}
	return 0, cause
}

// resumePoint is the offset an upload continues at, read from its update
// session: what the mixer has received while it is accepting, total once
// every byte is there.
func resumePoint(session json.RawMessage, total uint64) (uint64, *requestError) {
	state, ok := stringField(session, "state")
	if !ok {
		return 0, errProtocol()
	}
	received, ok := uintField(session, "received")
	if !ok || received > total {
		return 0, errProtocol()
	}
	switch state {
	case "accepting":
		return received, nil
	case "complete":
		if received != total {
			return 0, errProtocol()
		}
		return total, nil
	case "failed":
		return 0, statusError(409, errorDetail(session))
	default:
		return 0, errProtocol()
	}
}

// readError is the reason a file could not be read, without Go's operation
// and path prefix.
func readError(err error) error {
	var pathError *os.PathError
	if errors.As(err, &pathError) {
		return pathError.Err
	}
	return err
}

// driveTimeout limits how long the app waits to find the temporary update
// drive, copy the payload, and eject it before falling back to USB 1.
const driveTimeout = 90 * time.Second

// throughDrive asks the mixer to attach its temporary update drive, writes the
// payload with drive, and asks the mixer to read it back. Returns false (after
// releasing the drive if needed) on any failure so the caller can fall back to
// USB 1 control.
func (d device) throughDrive(ctx context.Context, path string, upload []byte, drive DriveWriter, copying func(total uint64)) bool {
	offer, failed := d.request(ctx, "POST", path+"/drive", nil)
	if failed != nil {
		return false
	}
	label, labelOK := stringField(offer, "label")
	name, nameOK := stringField(offer, "file")
	size, sizeOK := uintField(offer, "size")
	release := func() { d.request(ctx, "POST", path+"/drive/release", nil) }
	if !labelOK || !nameOK || !sizeOK || size != uint64(len(upload)) || !driveName(name) {
		release()
		return false
	}
	copying(size)
	writeCtx, cancel := context.WithTimeout(ctx, driveTimeout)
	err := drive(writeCtx, label, name, upload)
	cancel()
	if err != nil {
		release()
		return false
	}
	// If the mixer rejects the drive contents, it releases the drive itself and
	// waits for USB 1 chunks instead.
	taken, failed := d.request(ctx, "POST", path+"/drive/done", nil)
	if failed != nil {
		return false
	}
	received, ok := uintField(taken, "received")
	return ok && received == size
}

// driveName validates an uppercase 8.3 FAT filename (letters and digits only),
// matching what the mixer offers.
func driveName(name string) bool {
	base, ext, ok := strings.Cut(name, ".")
	valid := func(s string, most int) bool {
		if s == "" || len(s) > most {
			return false
		}
		for _, r := range s {
			if !(r >= 'A' && r <= 'Z' || r >= '0' && r <= '9') {
				return false
			}
		}
		return true
	}
	return ok && valid(base, 8) && valid(ext, 3)
}
