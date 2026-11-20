package installer

import (
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

type RequestKind int

const (
	RequestAllowExperimentalDuo RequestKind = iota
	RequestProbe
	RequestReleases
	RequestStockReleases
	RequestMixerReleases
	// RequestSelectMixer independently authenticates the user-selected Offer
	// against TUF metadata for FirmwareProfile.
	RequestSelectMixer
	// RequestSelectBundledMixer selects the compiled-in payload with no network fallback.
	RequestSelectBundledMixer
	// RequestInPlaceBundle stages a verified Offer and raw TUF metadata for an
	// in-place update (optional Installed version enables delta upload).
	RequestInPlaceBundle
	RequestSaveSettings
	RequestPrepare
	RequestEnterUSBMode
	RequestVerify
	RequestFlash
	// RequestRepairInspect reads a mixer's eMMC in bootloader mass-storage mode.
	RequestRepairInspect
	// RequestRepair backs up settings and restores Files after re-verifying each.
	RequestRepair
	// RequestStorageFix grows slot 1 via RØDE's USB update (rodecaster/autofix)
	// so future updates format with 4 KB blocks. With Final, the last flash uses
	// the prepared firmware instead of plain stock.
	RequestStorageFix
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
	RequestStorageFix:           "StorageFix",
}

// Request is an IPC command to Engine or the privileged helper.
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
	Final           bool
	// FixDebug enables the storage-fix script's on-mixer debug log (rc2fxdev builds only).
	FixDebug bool
}

type RepairTarget struct {
	Root uint8  `json:"root"` // 1 or 2
	Path string `json:"path"`
}

type RepairFindings struct {
	Refused    *string        `json:"refused"`
	Hostname   string         `json:"hostname"`
	SettingsOK bool           `json:"settings_ok"`
	Roots      []RootFindings `json:"roots"`
}

func (f *RepairFindings) Repairable() int {
	return len(f.Targets())
}

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

type RootFindings struct {
	Index     uint8         `json:"index"` // 1 or 2
	Reference string        `json:"reference"`
	Unclean   bool          `json:"unclean"`
	Damaged   []DamagedFile `json:"damaged"`
}

type DamagedFile struct {
	Path string  `json:"path"`
	Why  *string `json:"why"` // nil when repairable
}

type RepairResult struct {
	Backup      string      `json:"backup"`
	Repaired    []string    `json:"repaired"`
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

type storageFixBody struct {
	Final bool `json:"final"`
	Debug bool `json:"debug,default"`
}

type serialBody struct {
	Serial string `json:"serial"`
}

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
	case RequestStorageFix:
		return tagged(name, storageFixBody{r.Final, r.FixDebug})
	}
	return sjson.String(name), nil
}

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
	case RequestStorageFix:
		var body storageFixBody
		err = sjson.Unmarshal(payload, &body)
		out.Final, out.FixDebug = body.Final, body.Debug
	}
	if err != nil {
		return err
	}
	*r = out
	return nil
}

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
	ReplyStorageFix
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
	ReplyStorageFix:     "StorageFix",
	ReplyDone:           "Done",
}

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
	StorageFix    *StorageFix
}

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
	case ReplyStorageFix:
		return tagged(name, r.StorageFix)
	}
	return sjson.String(name), nil
}

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
	case ReplyStorageFix:
		out.StorageFix = new(StorageFix)
		err = sjson.Unmarshal(payload, out.StorageFix)
	}
	if err != nil {
		return err
	}
	*r = out
	return nil
}
