package core

// Image check report returned by GET api/v1/image-check.
//
// During flashing, the installer writes a SHA-256 list of RØDE's files into
// the image. On first boot, rc2fx.sh compares the rootfs against that list,
// repairs damaged files from the alternate root partition when possible, and
// writes key=value results to /opt/rc2fx/data/image-check. The desktop only
// reads the parsed report.

// ImageStatus uses serde camelCase; zero value is ImageStatusNone.
type ImageStatus int

const (
	ImageStatusNone     ImageStatus = iota // no file list in image (pre-check install or in-place update)
	ImageStatusPending                     // check hasn't finished yet
	ImageStatusOk                          // all listed files match
	ImageStatusRepaired                    // damaged files restored; reboot needed so RØDE loads them
	ImageStatusDamaged                     // at least one file is damaged and couldn't be restored
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

// ImageCheck allows unknown fields for forward compatibility with newer mixers.
type ImageCheck struct {
	Status   ImageStatus `json:"status,default"`
	Checked  *string     `json:"checked,omitempty"`
	Repaired []string    `json:"repaired,default"`
	Damaged  []string    `json:"damaged,default"`
	Unlisted uint        `json:"unlisted,default"` // count beyond the first 32 in either list
}

func (ImageCheck) AllowUnknownJSONFields() {}
