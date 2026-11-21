package installer

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

// Request is an IPC command to Engine or the privileged helper.
type Request struct {
	Kind            RequestKind    `json:"kind"`
	Allow           bool           `json:"allow,omitempty"`
	Offer           *MixerOffer    `json:"offer,omitempty"`
	FirmwareProfile string         `json:"firmware_profile,omitempty"`
	Installed       *string        `json:"installed,omitempty"`
	Serial          string         `json:"serial,omitempty"`
	Settings        string         `json:"settings,omitempty"`
	Channel         string         `json:"channel,omitempty"`
	Version         string         `json:"version,omitempty"`
	Stock           bool           `json:"stock,omitempty"`
	Files           []RepairTarget `json:"files,omitempty"`
	Final           bool           `json:"final,omitempty"`
	// FixDebug enables the storage-fix script's on-mixer debug log (rc2fxdev builds only).
	FixDebug bool `json:"fix_debug,omitempty"`
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

type Reply struct {
	Kind          ReplyKind       `json:"kind"`
	Status        *Status         `json:"status,omitempty"`
	Releases      []Release       `json:"releases,omitempty"`
	Offers        []MixerOffer    `json:"offers,omitempty"`
	Identity      *Identity       `json:"identity,omitempty"`
	InPlaceBundle *InPlaceBundle  `json:"in_place_bundle,omitempty"`
	Prepared      *Prepared       `json:"prepared,omitempty"`
	Backup        *BackupRecord   `json:"backup,omitempty"`
	Findings      *RepairFindings `json:"findings,omitempty"`
	Repaired      *RepairResult   `json:"repaired,omitempty"`
	StorageFix    *StorageFix     `json:"storage_fix,omitempty"`
}
