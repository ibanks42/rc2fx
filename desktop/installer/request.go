package installer

import (
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
)

// RequestKind names a Request variant.
type RequestKind int

const (
	// RequestAllowExperimentalDuo is session policy, sent explicitly by the
	// desktop's saved preference (Allow).
	RequestAllowExperimentalDuo RequestKind = iota
	RequestProbe
	RequestReleases
	// RequestStockReleases is the stock firmware RØDE publishes for the
	// connected model.
	RequestStockReleases
	// RequestMixerReleases discovers authenticated mixer releases. It does
	// not select or install one.
	RequestMixerReleases
	// RequestSelectMixer independently authenticates the exact offer
	// reviewed by the user (Offer, FirmwareProfile).
	RequestSelectMixer
	// RequestSelectBundledMixer explicitly selects the optional compiled-in
	// payload. No network fallback.
	RequestSelectBundledMixer
	// RequestInPlaceBundle downloads and verifies the signed Offer for an
	// in-place update of a mixer running FirmwareProfile, and reads the
	// release's metadata as published. Needs no administrator access and
	// touches no mixer. Installed is the RC2FX version the mixer runs, whose
	// signed payload the update is diffed against.
	RequestInPlaceBundle
	// RequestSaveSettings keeps Settings (RC2FX's settings file, read over
	// USB by the app) for the mixer Serial.
	RequestSaveSettings
	// RequestPrepare builds firmware with RC2FX and the newest settings saved
	// for Serial, or with Stock, stages the release unchanged.
	RequestPrepare
	RequestEnterUSBMode
	// RequestVerify checks the prepared firmware in the work folder against
	// this engine's payload and its checksum files, without touching the
	// mixer.
	RequestVerify
	RequestFlash
	// RequestRepairInspect looks over a mixer whose bootloader shows its
	// whole eMMC as a USB disk (see repair_linux.go). Reads only.
	RequestRepairInspect
	// RequestRepair backs up the settings partition to the work folder, then
	// restores Files, which a RequestRepairInspect found damaged and
	// repairable. The helper checks each again before writing it.
	RequestRepair
)

var requestNames = map[RequestKind]string{
	RequestAllowExperimentalDuo: "AllowExperimentalDuo",
	RequestProbe:                "Probe",
	RequestReleases:             "Releases",
	RequestStockReleases:        "StockReleases",
	RequestMixerReleases:        "MixerReleases",
	RequestSelectMixer:          "SelectMixer",
	RequestSelectBundledMixer:   "SelectBundledMixer",
	RequestInPlaceBundle:        "InPlaceBundle",
	RequestSaveSettings:         "SaveSettings",
	RequestPrepare:              "Prepare",
	RequestEnterUSBMode:         "EnterUsbMode",
	RequestVerify:               "Verify",
	RequestFlash:                "Flash",
	RequestRepairInspect:        "RepairInspect",
	RequestRepair:               "Repair",
}

// Request is what the app asks the engine (or its helper) to do. Only the
// fields of its Kind are used.
type Request struct {
	Kind            RequestKind
	Allow           bool
	Offer           *MixerOffer
	FirmwareProfile string
	Installed       *string
	Serial          string
	Settings        string
	Channel         string
	Version         string
	Stock           bool
	Files           []RepairTarget
}

// RepairTarget is a file to restore: system partition 1 or 2, and its path
// there.
type RepairTarget struct {
	Root uint8  `json:"root"`
	Path string `json:"path"`
}

// RepairFindings is what RequestRepairInspect found.
type RepairFindings struct {
	// Refused is why nothing can be repaired from here, if so; the rest is
	// then empty.
	Refused  *string `json:"refused"`
	Hostname string  `json:"hostname"`
	// SettingsOK: the settings partition's FAT looks sound.
	SettingsOK bool           `json:"settings_ok"`
	Roots      []RootFindings `json:"roots"`
}

// Repairable is the number of files a repair would restore.
func (f *RepairFindings) Repairable() int {
	return len(f.Targets())
}

// Targets is what to ask RequestRepair to restore.
func (f *RepairFindings) Targets() []RepairTarget {
	targets := []RepairTarget{}
	for _, r := range f.Roots {
		for _, d := range r.Damaged {
			if d.Why == nil {
				targets = append(targets, RepairTarget{Root: r.Index, Path: d.Path})
			}
		}
	}
	return targets
}

// RootFindings is one of the mixer's two system partitions.
type RootFindings struct {
	// Index is 1 or 2.
	Index uint8 `json:"index"`
	// Reference is what its files were compared with: the image's own list,
	// the image saved on this computer, or nothing.
	Reference string `json:"reference"`
	// Unclean: the partition was not shut down cleanly; nothing is written
	// to it.
	Unclean bool          `json:"unclean"`
	Damaged []DamagedFile `json:"damaged"`
}

type DamagedFile struct {
	Path string `json:"path"`
	// Why it can't be restored from here; nil when it can.
	Why *string `json:"why"`
}

// RepairResult is what RequestRepair did.
type RepairResult struct {
	// Backup is the settings partition backup on this computer.
	Backup   string   `json:"backup"`
	Repaired []string `json:"repaired"`
	// NotRepaired is the files left as they were, with the reason.
	NotRepaired [][2]string `json:"not_repaired"`
}

type repairBody struct {
	Files []RepairTarget `json:"files"`
}

type selectMixerBody struct {
	Offer           MixerOffer `json:"offer"`
	FirmwareProfile string     `json:"firmware_profile"`
}

type inPlaceBody struct {
	Offer           MixerOffer `json:"offer"`
	FirmwareProfile string     `json:"firmware_profile"`
	Installed       *string    `json:"installed,default"`
}

type saveSettingsBody struct {
	Serial   string `json:"serial"`
	Settings string `json:"settings"`
}

type prepareBody struct {
	Channel string `json:"channel"`
	Version string `json:"version"`
	Serial  string `json:"serial"`
	Stock   bool   `json:"stock"`
}

type serialBody struct {
	Serial string `json:"serial"`
}

// MarshalJSON writes serde's externally tagged form.
func (r Request) MarshalJSON() ([]byte, error) {
	name, ok := requestNames[r.Kind]
	if !ok {
		return nil, fmt.Errorf("unknown request %d", r.Kind)
	}
	offer := func() MixerOffer {
		if r.Offer == nil {
			return MixerOffer{}
		}
		return *r.Offer
	}
	switch r.Kind {
	case RequestAllowExperimentalDuo:
		return tagged(name, r.Allow)
	case RequestSelectMixer:
		return tagged(name, selectMixerBody{offer(), r.FirmwareProfile})
	case RequestInPlaceBundle:
		return tagged(name, inPlaceBody{offer(), r.FirmwareProfile, r.Installed})
	case RequestSaveSettings:
		return tagged(name, saveSettingsBody{r.Serial, r.Settings})
	case RequestPrepare:
		return tagged(name, prepareBody{r.Channel, r.Version, r.Serial, r.Stock})
	case RequestEnterUSBMode, RequestFlash:
		return tagged(name, serialBody{r.Serial})
	case RequestRepair:
		files := r.Files
		if files == nil {
			files = []RepairTarget{}
		}
		return tagged(name, repairBody{files})
	}
	return sjson.String(name), nil
}

// UnmarshalJSON reads serde's externally tagged form.
func (r *Request) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	kind := RequestKind(-1)
	for k, name := range requestNames {
		if name == variant {
			kind = k
		}
	}
	if kind < 0 {
		return fmt.Errorf("unknown variant `%s`", variant)
	}
	unit := kind == RequestProbe || kind == RequestReleases || kind == RequestStockReleases ||
		kind == RequestMixerReleases || kind == RequestSelectBundledMixer || kind == RequestVerify ||
		kind == RequestRepairInspect
	if unit != (payload == nil) {
		return fmt.Errorf("invalid type for variant `%s`", variant)
	}
	out := Request{Kind: kind}
	switch kind {
	case RequestAllowExperimentalDuo:
		err = sjson.Unmarshal(payload, &out.Allow)
	case RequestSelectMixer:
		var body selectMixerBody
		err = sjson.Unmarshal(payload, &body)
		out.Offer, out.FirmwareProfile = &body.Offer, body.FirmwareProfile
	case RequestInPlaceBundle:
		var body inPlaceBody
		err = sjson.Unmarshal(payload, &body)
		out.Offer, out.FirmwareProfile, out.Installed = &body.Offer, body.FirmwareProfile, body.Installed
	case RequestSaveSettings:
		var body saveSettingsBody
		err = sjson.Unmarshal(payload, &body)
		out.Serial, out.Settings = body.Serial, body.Settings
	case RequestPrepare:
		var body prepareBody
		err = sjson.Unmarshal(payload, &body)
		out.Channel, out.Version, out.Serial, out.Stock = body.Channel, body.Version, body.Serial, body.Stock
	case RequestEnterUSBMode, RequestFlash:
		var body serialBody
		err = sjson.Unmarshal(payload, &body)
		out.Serial = body.Serial
	case RequestRepair:
		var body repairBody
		err = sjson.Unmarshal(payload, &body)
		out.Files = body.Files
	}
	if err != nil {
		return err
	}
	*r = out
	return nil
}

// ReplyKind names a Reply variant.
type ReplyKind int

const (
	ReplyStatus ReplyKind = iota
	ReplyReleases
	ReplyStockReleases
	ReplyMixerReleases
	ReplyMixerSelected
	ReplyInPlaceBundle
	ReplyPrepared
	ReplyBackup
	ReplyRepairFindings
	ReplyRepaired
	ReplyDone
)

var replyNames = map[ReplyKind]string{
	ReplyStatus:         "Status",
	ReplyReleases:       "Releases",
	ReplyStockReleases:  "StockReleases",
	ReplyMixerReleases:  "MixerReleases",
	ReplyMixerSelected:  "MixerSelected",
	ReplyInPlaceBundle:  "InPlaceBundle",
	ReplyPrepared:       "Prepared",
	ReplyBackup:         "Backup",
	ReplyRepairFindings: "RepairFindings",
	ReplyRepaired:       "Repaired",
	ReplyDone:           "Done",
}

// Reply is a request's result. Only the field of its Kind is set:
// Releases for both release kinds, Offers for mixer releases.
type Reply struct {
	Kind          ReplyKind
	Status        *Status
	Releases      []Release
	Offers        []MixerOffer
	Identity      *Identity
	InPlaceBundle *InPlaceBundle
	Prepared      *Prepared
	Backup        *BackupRecord
	Findings      *RepairFindings
	Repaired      *RepairResult
}

// MarshalJSON writes serde's externally tagged form.
func (r Reply) MarshalJSON() ([]byte, error) {
	name, ok := replyNames[r.Kind]
	if !ok {
		return nil, fmt.Errorf("unknown reply %d", r.Kind)
	}
	switch r.Kind {
	case ReplyStatus:
		return tagged(name, r.Status)
	case ReplyReleases, ReplyStockReleases:
		return tagged(name, r.Releases)
	case ReplyMixerReleases:
		return tagged(name, r.Offers)
	case ReplyMixerSelected:
		return tagged(name, r.Identity)
	case ReplyInPlaceBundle:
		return tagged(name, r.InPlaceBundle)
	case ReplyPrepared:
		return tagged(name, r.Prepared)
	case ReplyBackup:
		return tagged(name, r.Backup)
	case ReplyRepairFindings:
		return tagged(name, r.Findings)
	case ReplyRepaired:
		return tagged(name, r.Repaired)
	}
	return sjson.String(name), nil
}

// UnmarshalJSON reads serde's externally tagged form.
func (r *Reply) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	kind := ReplyKind(-1)
	for k, name := range replyNames {
		if name == variant {
			kind = k
		}
	}
	if kind < 0 {
		return fmt.Errorf("unknown variant `%s`", variant)
	}
	if (kind == ReplyDone) != (payload == nil) {
		return fmt.Errorf("invalid type for variant `%s`", variant)
	}
	out := Reply{Kind: kind}
	switch kind {
	case ReplyStatus:
		out.Status = new(Status)
		err = sjson.Unmarshal(payload, out.Status)
	case ReplyReleases, ReplyStockReleases:
		err = sjson.Unmarshal(payload, &out.Releases)
	case ReplyMixerReleases:
		err = sjson.Unmarshal(payload, &out.Offers)
	case ReplyMixerSelected:
		out.Identity = new(Identity)
		err = sjson.Unmarshal(payload, out.Identity)
	case ReplyInPlaceBundle:
		out.InPlaceBundle = new(InPlaceBundle)
		err = sjson.Unmarshal(payload, out.InPlaceBundle)
	case ReplyPrepared:
		out.Prepared = new(Prepared)
		err = sjson.Unmarshal(payload, out.Prepared)
	case ReplyBackup:
		out.Backup = new(BackupRecord)
		err = sjson.Unmarshal(payload, out.Backup)
	case ReplyRepairFindings:
		out.Findings = new(RepairFindings)
		err = sjson.Unmarshal(payload, out.Findings)
	case ReplyRepaired:
		out.Repaired = new(RepairResult)
		err = sjson.Unmarshal(payload, out.Repaired)
	}
	if err != nil {
		return err
	}
	*r = out
	return nil
}
