// Package transport handles RC2FX control over USB 1 directly from the desktop
// process.
//
// With -tags mockmixer, a simulated mixer stands in for USB 1 during
// development; release builds always talk to a real mixer.
package transport

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// requestLimit is the largest request body the app sends.
const requestLimit = 512 * 1024

// versionReply reads a version reply; 404 means the runtime predates version
// reporting.
func versionReply(value json.RawMessage, failed *requestError) (*core.DeviceVersions, *requestError) {
	switch {
	case failed == nil && isObject(value):
		var versions core.DeviceVersions
		if decode(value, &versions) != nil {
			return nil, errProtocol()
		}
		if versions.Validate() != nil {
			return nil, errProtocol()
		}
		return &versions, nil
	case failed == nil:
		return nil, errProtocol()
	case failed.isStatus(404):
		return nil, nil
	default:
		return nil, failed
	}
}

// sshReply reads an SSH reply; 404 means the mixer software predates SSH
// management.
func sshReply(value json.RawMessage, failed *requestError) (*core.SSHStatus, *requestError) {
	switch {
	case failed == nil && isObject(value):
		var status core.SSHStatus
		if decode(value, &status) != nil {
			return nil, errProtocol()
		}
		if status.Validate() != nil {
			return nil, errProtocol()
		}
		return &status, nil
	case failed == nil:
		return nil, errProtocol()
	case failed.isStatus(404):
		return nil, nil
	default:
		return nil, failed
	}
}

func presetPath(id string, apply bool) (string, *requestError) {
	if !core.ValidID(id) {
		return "", errProtocol()
	}
	suffix := ""
	if apply {
		suffix = "/apply"
	}
	return "api/v1/presets/" + id + suffix, nil
}

func presetSummary(value json.RawMessage) (core.PresetSummary, *requestError) {
	var summary core.PresetSummary
	if decode(value, &summary) != nil {
		return core.PresetSummary{}, errProtocol()
	}
	if !core.ValidID(summary.ID) || strings.TrimSpace(summary.Name) == "" || len(summary.Name) > 128 {
		return core.PresetSummary{}, errProtocol()
	}
	return summary, nil
}

func presetList(value json.RawMessage) ([]core.PresetSummary, *requestError) {
	fields, ok := members(value)
	if !ok || len(fields) != 1 {
		return nil, errProtocol()
	}
	presets, ok := fields["presets"]
	if !ok {
		return nil, errProtocol()
	}
	list, ok := elements(presets)
	if !ok || len(list) > 128 {
		return nil, errProtocol()
	}
	ids := make(map[string]bool, len(list))
	summaries := make([]core.PresetSummary, 0, len(list))
	for _, item := range list {
		preset, failed := presetSummary(item)
		if failed != nil {
			return nil, failed
		}
		if ids[preset.ID] {
			return nil, errProtocol()
		}
		ids[preset.ID] = true
		summaries = append(summaries, preset)
	}
	return summaries, nil
}

// Transport runs the app's side of USB 1 on its own goroutines. Close it when
// its owner goes away.
type Transport struct {
	commands chan Command
	events   chan Event
	cancel   context.CancelFunc
	// Closed once the worker has stopped.
	done chan struct{}
}

// New starts the transport. The error is always nil: the goroutines cannot
// fail to start.
func New() (*Transport, error) {
	ctx, cancel := context.WithCancel(context.Background())
	t := &Transport{
		commands: make(chan Command, 16),
		events:   make(chan Event, 64),
		cancel:   cancel,
		done:     make(chan struct{}),
	}
	go func() {
		defer close(t.done)
		run(ctx, t.commands, t.events, connectDevice)
	}()
	return t, nil
}

// Send queues a command without waiting.
func (t *Transport) Send(command Command) error {
	select {
	case t.commands <- command:
		return nil
	default:
		return errors.New("Device connection is busy; try again shortly")
	}
}

// TryRecv returns the next event, if one is waiting.
func (t *Transport) TryRecv() (Event, bool) {
	select {
	case event := <-t.events:
		return event, true
	default:
		return Event{}, false
	}
}

// Close stops the transport without waiting for it.
func (t *Transport) Close() {
	// Do not drain queued writes when the window owner is gone. Canceling
	// stops outstanding operations and the event poll. A write already
	// received by the mixer cannot be recalled here.
	t.cancel()
}

type device struct {
	usb Usb
}

func (d device) clone() device {
	return device{d.usb.clone()}
}

func (d device) release() {
	d.usb.release()
}

// connectDevice connects and reads the state; also returns the mixer's USB
// serial.
func connectDevice(ctx context.Context) (device, string, json.RawMessage, error) {
	usb, mixer, err := connectUSB(ctx)
	if err != nil {
		return device{}, "", nil, err
	}
	d := device{usb}
	state, failed := d.request(ctx, "GET", "api/v1/state", nil)
	if failed != nil {
		d.release()
		return device{}, "", nil, errors.New(failed.message())
	}
	return d, mixer, state, nil
}

func (d device) preset(ctx context.Context, action PresetAction) (PresetReply, *requestError) {
	switch action := action.(type) {
	case PresetActionList:
		value, failed := d.request(ctx, "GET", "api/v1/presets", nil)
		if failed != nil {
			return nil, failed
		}
		list, failed := presetList(value)
		if failed != nil {
			return nil, failed
		}
		return PresetReplyList{list}, nil
	case PresetActionCreate:
		name := strings.TrimSpace(action.Name)
		if name == "" || len(name) > 128 {
			return nil, errProtocol()
		}
		body := object(
			member{"name", jsonString(name)},
			member{"expectedRevision", jsonUint(action.ExpectedRevision)},
		)
		value, failed := d.request(ctx, "POST", "api/v1/presets", body)
		if failed != nil {
			return nil, failed
		}
		summary, failed := presetSummary(value)
		if failed != nil {
			return nil, failed
		}
		return PresetReplyCreated{summary}, nil
	case PresetActionApply:
		path, failed := presetPath(action.ID, true)
		if failed != nil {
			return nil, failed
		}
		body := object(member{"expectedRevision", jsonUint(action.ExpectedRevision)})
		value, failed := d.request(ctx, "POST", path, body)
		if failed != nil {
			return nil, failed
		}
		return PresetReplyApplied{value}, nil
	case PresetActionDelete:
		path, failed := presetPath(action.ID, false)
		if failed != nil {
			return nil, failed
		}
		value, failed := d.request(ctx, "DELETE", path, nil)
		if failed != nil {
			return nil, failed
		}
		fields, ok := members(value)
		deleted, isString := asString(fields["deleted"])
		if !ok || len(fields) != 1 || !isString || deleted != action.ID {
			return nil, errProtocol()
		}
		return PresetReplyDeleted{action.ID}, nil
	default:
		return nil, errProtocol()
	}
}

// transition is a lifecycle change the mixer accepts with 202, returning its
// state.
func (d device) transition(ctx context.Context, method, path string, request any) (core.DeviceState, *requestError) {
	body, err := marshal(request)
	if err != nil {
		return core.DeviceState{}, errProtocol()
	}
	value, failed := d.requestWithStatus(ctx, method, path, body, 202)
	if failed != nil {
		return core.DeviceState{}, failed
	}
	text, err := compact(value)
	if err != nil {
		return core.DeviceState{}, errProtocol()
	}
	state, err := core.DeviceStateFromJSON(string(text))
	if err != nil {
		return core.DeviceState{}, errProtocol()
	}
	return state, nil
}

func (d device) request(ctx context.Context, method, path string, body json.RawMessage) (json.RawMessage, *requestError) {
	return d.requestWithStatus(ctx, method, path, body, 0)
}

// requestWithStatus is a request whose reply must be a success; with
// expectedStatus other than 0, exactly that one.
func (d device) requestWithStatus(ctx context.Context, method, path string, body json.RawMessage, expectedStatus uint16) (json.RawMessage, *requestError) {
	reply, failed := d.usb.request(ctx, method, path, body)
	if failed != nil {
		return nil, failed
	}
	if reply.Status < 200 || reply.Status >= 300 {
		return nil, statusError(reply.Status, errorDetail(reply.Body))
	}
	if expectedStatus != 0 && expectedStatus != reply.Status {
		return nil, errProtocol()
	}
	return reply.Body, nil
}

// errorDetail is a reply's `error` text, if it has one.
func errorDetail(body json.RawMessage) *string {
	if detail, ok := stringField(body, "error"); ok {
		return &detail
	}
	return nil
}

type requestKind int

const (
	requestUSB requestKind = iota
	requestStatus
	requestTooLarge
	requestProtocol
)

type requestError struct {
	kind requestKind
	// The USB error's text.
	text string
	// A refused request's status and the mixer's reason, if it gave one.
	status uint16
	detail *string
}

func usbError(text string) *requestError {
	return &requestError{kind: requestUSB, text: text}
}

func statusError(status uint16, detail *string) *requestError {
	return &requestError{kind: requestStatus, status: status, detail: detail}
}

func errTooLarge() *requestError {
	return &requestError{kind: requestTooLarge}
}

func errProtocol() *requestError {
	return &requestError{kind: requestProtocol}
}

func (e *requestError) isStatus(status uint16) bool {
	return e != nil && e.kind == requestStatus && e.status == status
}

func (e *requestError) message() string {
	switch e.kind {
	case requestUSB:
		return e.text
	case requestStatus:
		switch {
		case e.status == 409 && e.detail != nil && *e.detail == "revision conflict":
			return "The mixer's settings changed. Reload them before applying this draft."
		case e.detail != nil:
			return *e.detail
		default:
			return fmt.Sprintf("The mixer refused the request (%d)", e.status)
		}
	case requestTooLarge:
		return "The message is too large for the mixer"
	default:
		return "The mixer sent an invalid control message"
	}
}

func (e *requestError) Error() string {
	return e.message()
}

// asError is the user's message for a failed request, or nil.
func asError(failed *requestError) error {
	if failed == nil {
		return nil
	}
	return errors.New(failed.message())
}

func emit(ctx context.Context, events chan<- Event, generation uint64, kind EventKind) bool {
	select {
	case events <- Event{Generation: generation, Kind: kind}:
		return true
	case <-ctx.Done():
		return false
	}
}

// sleep waits d; false when ctx ended first.
func sleep(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-timer.C:
		return true
	case <-ctx.Done():
		return false
	}
}

func noMixer() *requestError {
	return usbError("No connected mixer")
}

// mixerConnection is one connection to the mixer: its USB session and the
// goroutines that use it beside the command loop, the event stream and an
// in-place update. They all run under ctx, so close ends them together and
// nothing of one connection outlives it or shares the USB session with the
// next.
type mixerConnection struct {
	device
	ctx    context.Context
	cancel context.CancelFunc
	// tasks counts the goroutines started with spawn.
	tasks sync.WaitGroup
	// update is closed when the in-place update started on this connection
	// ends; nil while none was started.
	update chan struct{}
}

func newConnection(ctx context.Context, d device) *mixerConnection {
	ctx, cancel := context.WithCancel(ctx)
	return &mixerConnection{device: d, ctx: ctx, cancel: cancel}
}

// spawn runs task on its own goroutine with its own hold on the USB session,
// until the task returns or the connection closes.
func (c *mixerConnection) spawn(task func(ctx context.Context, d device)) {
	d := c.device.clone()
	c.tasks.Add(1)
	go func() {
		defer c.tasks.Done()
		defer d.release()
		task(c.ctx, d)
	}()
}

// updating reports an in-place update still running on this connection.
func (c *mixerConnection) updating() bool {
	if c.update == nil {
		return false
	}
	select {
	case <-c.update:
		return false
	default:
		return true
	}
}

// startUpdate sends command's release on its own goroutine, reporting
// progress as generation's EventKindMixerUpdate. Only one runs per
// connection: starting another update session on the mixer replaces the
// one the first is still uploading to.
func (c *mixerConnection) startUpdate(command CommandMixerUpdate, generation uint64, events chan<- Event) {
	done := make(chan struct{})
	c.update = done
	c.spawn(func(ctx context.Context, d device) {
		progress := func(sent, total uint64) {
			emit(ctx, events, generation, EventKindMixerUpdate{MixerUpdateEventUploading{Sent: sent, Total: total}})
		}
		var kind MixerUpdateEvent = MixerUpdateEventInstalling{}
		copying := func(total uint64) {
			emit(ctx, events, generation, EventKindMixerUpdate{MixerUpdateEventCopying{Total: total}})
		}
		if failed := d.mixerUpdate(ctx, command.Start, command.Payload, command.Base, command.Drive, copying, progress); failed != nil {
			kind = MixerUpdateEventFailed{failed.message()}
		}
		// Ended before it says so: once the app reads how it ended, another
		// update may start.
		close(done)
		emit(ctx, events, generation, EventKindMixerUpdate{kind})
	})
}

// close cancels everything running on the connection, waits for it to let
// go of the USB session, then lets go itself. Every task ends promptly on
// cancel: each wait in it also waits on its context. Once the last hold is
// released, the session's goroutine finishes the request it may be serving,
// skips any queued, and closes the handle.
func (c *mixerConnection) close() {
	c.cancel()
	c.tasks.Wait()
	c.device.release()
}

// connector opens a connection to the mixer and reads its state:
// connectDevice, or a stand-in in tests.
type connector func(ctx context.Context) (device, string, json.RawMessage, error)

func run(ctx context.Context, commands <-chan Command, events chan<- Event, connect connector) {
	var current *mixerConnection
	generation := uint64(0)
	// Connect and Disconnect end the connection and everything on it,
	// including an in-place update still uploading: its events would belong
	// to a generation the app no longer reads.
	drop := func() {
		if current != nil {
			current.close()
			current = nil
		}
	}
	defer drop()
	for {
		// Cancellation wins over queued commands: they are not drained.
		if ctx.Err() != nil {
			return
		}
		var command Command
		select {
		case <-ctx.Done():
			return
		case next, ok := <-commands:
			if !ok {
				return
			}
			command = next
		}
		switch command := command.(type) {
		case CommandSelection:
			if command.Generation != generation {
				continue
			}
			state, failed := core.DeviceState{}, noMixer()
			if current != nil {
				state, failed = current.transition(ctx, "PUT", "api/v1/selection", command.Request)
			}
			var err error
			if failed.isStatus(409) {
				err = errors.New("The mixer state changed or a transition is already pending. The path change was not retried.")
			} else {
				err = asError(failed)
			}
			emit(ctx, events, generation, EventKindSelection{State: state, Err: err})
		case CommandDiagnostics:
			if command.Generation != generation {
				continue
			}
			report, failed := json.RawMessage(nil), noMixer()
			if current != nil {
				report, failed = current.request(ctx, "GET", "api/v1/diagnostics", nil)
			}
			emit(ctx, events, generation, EventKindDiagnostics{Report: report, Err: asError(failed)})
		case CommandResetDiagnostics:
			if command.Generation != generation {
				continue
			}
			report, failed := json.RawMessage(nil), noMixer()
			if current != nil {
				report, failed = current.request(ctx, "POST", "api/v1/diagnostics/reset", nil)
			}
			emit(ctx, events, generation, EventKindResetDiagnostics{Report: report, Err: asError(failed)})
		case CommandDiagnosticsEvents:
			if command.Generation != generation {
				continue
			}
			path := fmt.Sprintf("api/v1/diagnostics/events?after=%d", command.After)
			var page core.DiagnosticsEvents
			failed := noMixer()
			if current != nil {
				var value json.RawMessage
				value, failed = current.request(ctx, "GET", path, nil)
				if failed == nil {
					if err := decode(value, &page); err != nil {
						text := err.Error()
						failed = statusError(500, &text)
					}
				}
			}
			emit(ctx, events, generation, EventKindDiagnosticsEvents{Events: page, Err: asError(failed)})
		case CommandSettings:
			if command.Generation != generation {
				continue
			}
			settings, failed := "", noMixer()
			if current != nil {
				var value json.RawMessage
				value, failed = current.request(ctx, "GET", "api/v1/settings", nil)
				if failed == nil {
					text, err := compact(value)
					if err != nil {
						failed = errProtocol()
					}
					settings = string(text)
				}
			}
			emit(ctx, events, generation, EventKindSettings{Settings: settings, Err: asError(failed)})
		case CommandVersion:
			if command.Generation != generation {
				continue
			}
			var versions *core.DeviceVersions
			failed := noMixer()
			if current != nil {
				versions, failed = versionReply(current.request(ctx, "GET", "api/v1/version", nil))
			}
			emit(ctx, events, generation, EventKindVersion{Versions: versions, Err: asError(failed)})
		case CommandSSH:
			if command.Generation != generation {
				continue
			}
			var status *core.SSHStatus
			failed := noMixer()
			if current != nil {
				status, failed = sshReply(current.request(ctx, "GET", "api/v1/ssh", nil))
			}
			emit(ctx, events, generation, EventKindSSH{ID: command.ID, Status: status, Err: asError(failed)})
		case CommandSetSSH:
			if command.Generation != generation {
				continue
			}
			var status *core.SSHStatus
			failed := noMixer()
			if current != nil {
				if body, err := marshal(command.Request); err != nil {
					failed = errProtocol()
				} else {
					status, failed = sshReply(current.request(ctx, "PUT", "api/v1/ssh", body))
				}
			}
			emit(ctx, events, generation, EventKindSSH{ID: command.ID, Status: status, Err: asError(failed)})
		case CommandMixerUpdate:
			if command.Generation != generation {
				continue
			}
			if current == nil {
				failed := MixerUpdateEventFailed{noMixer().message()}
				emit(ctx, events, generation, EventKindMixerUpdate{failed})
				continue
			}
			if current.updating() {
				failed := MixerUpdateEventFailed{"Another update is still being sent to the mixer"}
				emit(ctx, events, generation, EventKindMixerUpdate{failed})
				continue
			}
			// The upload runs beside other requests; the mixer restarts once
			// it installs, which ends this connection anyway.
			current.startUpdate(command, generation, events)
		case CommandMixerUpdateLast:
			if command.Generation != generation {
				continue
			}
			var last json.RawMessage
			failed := noMixer()
			if current != nil {
				var value json.RawMessage
				value, failed = current.request(ctx, "GET", "api/v1/update/last", nil)
				switch fields, ok := members(value); {
				case failed == nil && ok && len(fields) == 0:
				case failed == nil && ok:
					last = value
				case failed == nil:
					failed = errProtocol()
				case failed.isStatus(404):
					failed = nil
				}
			}
			emit(ctx, events, generation, EventKindMixerUpdateLast{Last: last, Err: asError(failed)})
		case CommandImageCheck:
			if command.Generation != generation {
				continue
			}
			var check *core.ImageCheck
			failed := noMixer()
			if current != nil {
				var value json.RawMessage
				value, failed = current.request(ctx, "GET", "api/v1/image-check", nil)
				switch {
				case failed == nil:
					check = new(core.ImageCheck)
					if decode(value, check) != nil {
						check, failed = nil, errProtocol()
					}
				case failed.isStatus(404):
					failed = nil
				}
			}
			emit(ctx, events, generation, EventKindImageCheck{Check: check, Err: asError(failed)})
		case CommandInputs:
			if command.Generation != generation {
				continue
			}
			var inventory core.StockInventory
			failed := noMixer()
			if current != nil {
				var value json.RawMessage
				value, failed = current.request(ctx, "GET", "api/v1/inputs", nil)
				if failed == nil {
					if decode(value, &inventory) != nil || inventory.Validate() != nil {
						inventory = core.StockInventory{}
						failed = errProtocol()
					}
				}
			}
			emit(ctx, events, generation, EventKindInputs{Inventory: inventory, Err: asError(failed)})
		case CommandRouting:
			if command.Generation != generation {
				continue
			}
			state, failed := core.DeviceState{}, noMixer()
			if current != nil {
				state, failed = current.transition(ctx, "PUT", "api/v1/routing", command.Request)
			}
			emit(ctx, events, generation, EventKindRouting{State: state, Err: asError(failed)})
		case CommandPreset:
			if command.Generation != generation {
				continue
			}
			var reply PresetReply
			failed := noMixer()
			if current != nil {
				reply, failed = current.preset(ctx, command.Action)
			}
			var presetError *PresetError
			if failed != nil {
				presetError = &PresetError{Message: failed.message(), Conflict: failed.isStatus(409)}
			}
			emit(ctx, events, generation, EventKindPreset{Reply: reply, Err: presetError})
		case CommandConnect:
			drop()
			generation = command.Generation
			d, mixer, state, err := connect(ctx)
			if err != nil {
				emit(ctx, events, generation, EventKindFailure{err.Error()})
				continue
			}
			if !emit(ctx, events, generation, EventKindConnected{Mixer: mixer, State: state}) {
				d.release()
				return
			}
			current = newConnection(ctx, d)
			generation := generation
			current.spawn(func(ctx context.Context, d device) {
				streamEvents(ctx, d, generation, events)
			})
		case CommandDisconnect:
			drop()
			generation = command.Generation
		case CommandApply:
			if command.Generation != generation || current == nil {
				continue
			}
			var kind EventKind
			state, failed := current.request(ctx, "PUT", "api/v1/config", applyBody(command.Update))
			switch {
			case failed == nil:
				kind = EventKindApplied{state}
			case failed.isStatus(409):
				kind = EventKindConflict{}
			default:
				kind = EventKindFailure{failed.message()}
			}
			emit(ctx, events, generation, kind)
		case CommandLive:
			if command.Generation != generation || current == nil {
				continue
			}
			kind := EventKindLiveApplied{Result: LiveAccepted}
			if _, failed := current.request(ctx, "PUT", "api/v1/live", applyBody(command.Update)); failed != nil {
				switch {
				case failed.isStatus(404):
					kind.Result = LiveUnsupported
				case failed.isStatus(422):
					kind.Result = LiveNeedsSave
				case failed.isStatus(409):
					kind.Result = LiveConflict
				case failed.isStatus(503):
					kind.Result = LiveBusy
				default:
					kind.Result = LiveFailed
				}
				kind.Message = failed.message()
			}
			emit(ctx, events, generation, kind)
		case CommandRefresh:
			if command.Generation != generation || current == nil {
				continue
			}
			var kind EventKind
			if state, failed := current.request(ctx, "GET", "api/v1/state", nil); failed == nil {
				kind = EventKindRefreshed{state}
			} else {
				kind = EventKindFailure{failed.message()}
			}
			emit(ctx, events, generation, kind)
		}
	}
}

// streamEvents polls the mixer's events, backing off while the poll fails. A
// poll that worked for a while starts the backoff again from one second.
func streamEvents(ctx context.Context, d device, generation uint64, events chan<- Event) {
	delay := 1
	for {
		started := time.Now()
		message := "Device event stream closed"
		if err := readStream(ctx, d, generation, events); err != nil {
			message = err.Error()
		}
		if time.Since(started) > 30*time.Second {
			delay = 1
		}
		if !emit(ctx, events, generation, EventKindStreamStatus{Connected: false, Message: message}) {
			return
		}
		if !sleep(ctx, time.Duration(delay)*time.Second) {
			return
		}
		delay = min(delay*2, 15)
	}
}

func readStream(ctx context.Context, d device, generation uint64, events chan<- Event) error {
	announced := false
	for {
		value, failed := d.request(ctx, "GET", "api/v1/events", nil)
		if failed != nil {
			return errors.New(failed.message())
		}
		if !announced {
			if !emit(ctx, events, generation, EventKindStreamStatus{Connected: true, Message: "Connected"}) {
				return nil
			}
			announced = true
		}
		if !isNull(value) {
			name, ok := stringField(value, "event")
			if !ok {
				return errors.New("Invalid USB event")
			}
			data, ok := field(value, "data")
			if !ok {
				return errors.New("Invalid USB event data")
			}
			var kind EventKind
			switch name {
			case "state":
				kind = EventKindState{data}
			case "meter":
				kind = EventKindMeter{data}
			case "usb":
				kind = EventKindUSB{data}
			default:
				return errors.New("Unknown USB event")
			}
			if !emit(ctx, events, generation, kind) {
				return nil
			}
		}
		if !sleep(ctx, 50*time.Millisecond) {
			// Canceled; the caller's next emit sees it too.
			return errors.New("Device event stream closed")
		}
	}
}

// applyBody is the body of a settings save: the complete typed update.
func applyBody(update core.ConfigUpdate) json.RawMessage {
	body, err := marshal(update)
	if err != nil {
		panic("typed config serializes")
	}
	return body
}
