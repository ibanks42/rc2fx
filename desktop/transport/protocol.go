package transport

import (
	"encoding/json"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Command is a request to the transport, sent with Transport.Send. Each
// carries the generation it belongs to; a command for any other generation
// than the current one is dropped, except Connect and Disconnect, which start
// a new one.
type Command interface {
	command()
}

type CommandSelection struct {
	Generation uint64
	Request    core.SelectionRequest
}

type CommandDiagnostics struct {
	Generation uint64
}

// CommandResetDiagnostics restarts the diagnostics baselines and the event
// log.
type CommandResetDiagnostics struct {
	Generation uint64
}

// CommandDiagnosticsEvents reads one page of the diagnostics event log, from
// After (a sequence number; 0 for everything the mixer still holds).
type CommandDiagnosticsEvents struct {
	Generation uint64
	After      uint64
}

type CommandInputs struct {
	Generation uint64
}

type CommandRouting struct {
	Generation uint64
	Request    core.RoutingRequest
}

type CommandPreset struct {
	Generation uint64
	Action     PresetAction
}

// CommandConnect connects to the mixer on USB 1.
type CommandConnect struct {
	Generation uint64
}

type CommandDisconnect struct {
	Generation uint64
}

type CommandApply struct {
	Generation uint64
	Update     core.ConfigUpdate
}

type CommandRefresh struct {
	Generation uint64
}

// CommandLive puts Update on the running audio at once (PUT api/v1/live):
// live editing while a control moves. The app sends one at a time, the
// newest edit, and saves with CommandApply once edits stop; if the app goes
// away first, the mixer keeps and saves what plays.
type CommandLive struct {
	Generation uint64
	Update     core.ConfigUpdate
}

// CommandSettings reads RC2FX's settings file, for the installer to keep.
type CommandSettings struct {
	Generation uint64
}

// CommandVersion reads the versions the mixer reports. Older runtimes do not
// report any.
type CommandVersion struct {
	Generation uint64
}

// CommandSSH reads SSH access on the mixer. ID comes back with the reply.
type CommandSSH struct {
	Generation uint64
	ID         uint64
}

// CommandSetSSH adds or removes RC2FX's managed SSH key on the mixer.
type CommandSetSSH struct {
	Generation uint64
	ID         uint64
	Request    core.SSHRequest
}

// CommandMixerUpdate sends a signed release to the mixer and starts
// installing it in place. Start is the body of `POST api/v1/update`; Payload
// the release file it describes, and Base the signed release the mixer runs,
// if any, to send only what changed. Progress arrives as
// EventKindMixerUpdate.
type CommandMixerUpdate struct {
	Generation uint64
	Start      json.RawMessage
	Payload    string
	Base       *string
}

// CommandMixerUpdateLast reads how the last in-place update ended.
type CommandMixerUpdateLast struct {
	Generation uint64
}

// CommandImageCheck reads the mixer's check of the files the last flash
// unpacked.
type CommandImageCheck struct {
	Generation uint64
}

func (CommandSelection) command()         {}
func (CommandDiagnostics) command()       {}
func (CommandResetDiagnostics) command()  {}
func (CommandDiagnosticsEvents) command() {}
func (CommandInputs) command()            {}
func (CommandRouting) command()           {}
func (CommandPreset) command()            {}
func (CommandConnect) command()           {}
func (CommandDisconnect) command()        {}
func (CommandApply) command()             {}
func (CommandLive) command()              {}
func (CommandRefresh) command()           {}
func (CommandSettings) command()          {}
func (CommandVersion) command()           {}
func (CommandSSH) command()               {}
func (CommandSetSSH) command()            {}
func (CommandMixerUpdate) command()       {}
func (CommandMixerUpdateLast) command()   {}
func (CommandImageCheck) command()        {}

// MixerUpdateEvent is how an in-place update is going, as far as this side
// can see. Once the mixer restarts, CommandMixerUpdateLast tells how it
// ended. Its variants are comparable with ==.
type MixerUpdateEvent interface {
	mixerUpdateEvent()
}

type MixerUpdateEventUploading struct {
	Sent  uint64
	Total uint64
}

// MixerUpdateEventInstalling: verified and accepted; the mixer is installing
// and will restart.
type MixerUpdateEventInstalling struct{}

type MixerUpdateEventFailed struct {
	Message string
}

func (MixerUpdateEventUploading) mixerUpdateEvent()  {}
func (MixerUpdateEventInstalling) mixerUpdateEvent() {}
func (MixerUpdateEventFailed) mixerUpdateEvent()     {}

type Event struct {
	Generation uint64
	Kind       EventKind
}

// EventKind is what an Event reports. A variant with an Err field carries
// either its value or, when Err is not nil, the message the user sees.
type EventKind interface {
	eventKind()
}

type EventKindSelection struct {
	State core.DeviceState
	Err   error
}

type EventKindDiagnostics struct {
	Report json.RawMessage
	Err    error
}

type EventKindResetDiagnostics struct {
	Report json.RawMessage
	Err    error
}

type EventKindDiagnosticsEvents struct {
	Events core.DiagnosticsEvents
	Err    error
}

type EventKindSettings struct {
	Settings string
	Err      error
}

// EventKindVersion: Versions nil and Err nil means this runtime predates
// version reporting.
type EventKindVersion struct {
	Versions *core.DeviceVersions
	Err      error
}

// EventKindSSH is the reply to the SSH request with this ID. Status nil and
// Err nil means the mixer software predates SSH management.
type EventKindSSH struct {
	ID     uint64
	Status *core.SSHStatus
	Err    error
}

type EventKindInputs struct {
	Inventory core.StockInventory
	Err       error
}

type EventKindMixerUpdate struct {
	Event MixerUpdateEvent
}

// EventKindImageCheck: Check nil and Err nil means the mixer software
// predates the image check.
type EventKindImageCheck struct {
	Check *core.ImageCheck
	Err   error
}

// EventKindMixerUpdateLast: Last nil and Err nil means no in-place update has
// finished on this mixer, or its software predates them.
type EventKindMixerUpdateLast struct {
	Last json.RawMessage
	Err  error
}

type EventKindRouting struct {
	State core.DeviceState
	Err   error
}

type EventKindPreset struct {
	Reply PresetReply
	Err   *PresetError
}

// EventKindConnected: connected to the mixer with this USB serial; its state.
type EventKindConnected struct {
	Mixer string
	State json.RawMessage
}

type EventKindApplied struct {
	State json.RawMessage
}

type EventKindState struct {
	State json.RawMessage
}

type EventKindRefreshed struct {
	State json.RawMessage
}

type EventKindMeter struct {
	Meter json.RawMessage
}

type EventKindUSB struct {
	USB json.RawMessage
}

type EventKindStreamStatus struct {
	Connected bool
	Message   string
}

type EventKindFailure struct {
	Message string
}

type EventKindConflict struct{}

// LiveResult is how the mixer took a live edit.
type LiveResult int

const (
	// LiveAccepted: the audio has (or will next have) these settings.
	LiveAccepted LiveResult = iota
	// LiveUnsupported: the mixer has no live editing (404, an older mixer);
	// the app saves instead.
	LiveUnsupported
	// LiveNeedsSave: the change needs the engine restarted (422
	// liveRequiresSave), so only a save can make it.
	LiveNeedsSave
	// LiveConflict: the saved revision moved on (409).
	LiveConflict
	// LiveBusy: the engine is not running right now (503); try again.
	LiveBusy
	// LiveFailed: any other failure; Message says what.
	LiveFailed
)

// EventKindLiveApplied is a CommandLive's answer.
type EventKindLiveApplied struct {
	Result  LiveResult
	Message string
}

func (EventKindSelection) eventKind()         {}
func (EventKindDiagnostics) eventKind()       {}
func (EventKindResetDiagnostics) eventKind()  {}
func (EventKindDiagnosticsEvents) eventKind() {}
func (EventKindSettings) eventKind()          {}
func (EventKindVersion) eventKind()           {}
func (EventKindSSH) eventKind()               {}
func (EventKindInputs) eventKind()            {}
func (EventKindMixerUpdate) eventKind()       {}
func (EventKindMixerUpdateLast) eventKind()   {}
func (EventKindImageCheck) eventKind()        {}
func (EventKindRouting) eventKind()           {}
func (EventKindPreset) eventKind()            {}
func (EventKindConnected) eventKind()         {}
func (EventKindApplied) eventKind()           {}
func (EventKindState) eventKind()             {}
func (EventKindRefreshed) eventKind()         {}
func (EventKindMeter) eventKind()             {}
func (EventKindUSB) eventKind()               {}
func (EventKindStreamStatus) eventKind()      {}
func (EventKindFailure) eventKind()           {}
func (EventKindConflict) eventKind()          {}
func (EventKindLiveApplied) eventKind()       {}

type PresetAction interface {
	presetAction()
}

type PresetActionList struct{}

type PresetActionCreate struct {
	Name             string
	ExpectedRevision uint64
}

type PresetActionApply struct {
	ID               string
	ExpectedRevision uint64
}

type PresetActionDelete struct {
	ID string
}

func (PresetActionList) presetAction()   {}
func (PresetActionCreate) presetAction() {}
func (PresetActionApply) presetAction()  {}
func (PresetActionDelete) presetAction() {}

type PresetReply interface {
	presetReply()
}

type PresetReplyList struct {
	Presets []core.PresetSummary
}

type PresetReplyCreated struct {
	Preset core.PresetSummary
}

type PresetReplyApplied struct {
	State json.RawMessage
}

type PresetReplyDeleted struct {
	ID string
}

func (PresetReplyList) presetReply()    {}
func (PresetReplyCreated) presetReply() {}
func (PresetReplyApplied) presetReply() {}
func (PresetReplyDeleted) presetReply() {}

type PresetError struct {
	Message  string
	Conflict bool
}

func (e *PresetError) Error() string {
	return e.Message
}
