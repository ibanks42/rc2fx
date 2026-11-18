package core

// The mixer's check of the files a flash unpacked (`rc2fx.sh image-check`),
// as `GET api/v1/image-check` reports it.
//
// The installer lists RØDE's files with their SHA-256 in the image. On the
// first boots the launcher compares the root against that list, restores a
// damaged file from the other root partition when that copy is identical,
// and records the outcome as `key=value` lines in `/opt/rc2fx/data/image-check`.
// The mixer turns that record into the report (Rust ImageCheck::from_record);
// the desktop only reads the report.

// ImageStatus is serde `rename_all = "camelCase"`. The zero value is
// ImageStatusNone, ImageCheck's default.
type ImageStatus int

const (
	// ImageStatusNone: the image has no file list (a mixer installed before
	// the check, or after an in-place update): there is nothing to check.
	ImageStatusNone ImageStatus = iota
	// ImageStatusPending: the list is there and the check has not finished
	// yet.
	ImageStatusPending
	// ImageStatusOk: every listed file matches.
	ImageStatusOk
	// ImageStatusRepaired: damaged files were restored; RØDE's boot already
	// used the damaged copies, so the mixer needs a restart.
	ImageStatusRepaired
	// ImageStatusDamaged: at least one listed file is damaged and could not
	// be restored.
	ImageStatusDamaged
)

var imageStatusNames = []string{"none", "pending", "ok", "repaired", "damaged"}

func (s ImageStatus) String() string {
	if s < 0 || int(s) >= len(imageStatusNames) {
		return "ImageStatus(?)"
	}
	return imageStatusNames[s]
}

func (s *ImageStatus) UnmarshalText(text []byte) error {
	var name string
	if err := enumUnmarshal(&name, text, imageStatusNames); err != nil {
		return err
	}
	for i, n := range imageStatusNames {
		if n == name {
			*s = ImageStatus(i)
		}
	}
	return nil
}

// ImageCheck is serde `rename_all = "camelCase", default`, without
// deny_unknown_fields: an older app reading a newer field set still parses.
type ImageCheck struct {
	Status   ImageStatus `json:"status,default"`
	Checked  *string     `json:"checked,omitempty"`
	Repaired []string    `json:"repaired,default"`
	Damaged  []string    `json:"damaged,default"`
	// Unlisted is the files beyond the first 32 in either list.
	Unlisted uint `json:"unlisted,default"`
}

// AllowUnknownJSONFields: ImageCheck has no deny_unknown_fields.
func (ImageCheck) AllowUnknownJSONFields() {}
