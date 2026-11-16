package core

// The mixer's check of the files a flash unpacked (`rc2fx.sh image-check`),
// as `GET api/v1/image-check` reports it.
//
// The installer lists RØDE's files with their SHA-256 in the image. On the
// first boots the launcher compares the root against that list, restores a
// damaged file from the other root partition when that copy is identical,
// and records the outcome as `key=value` lines in `/opt/rc2fx/data/image-check`.

import "strings"

// MaxListed is how many file names a report carries; the rest are counted,
// not listed.
const MaxListed = 32

// maxRecord is the largest record read.
const maxRecord = 64 * 1024

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

func (s ImageStatus) MarshalText() ([]byte, error) {
	if s < 0 || int(s) >= len(imageStatusNames) {
		return nil, CoreError("invalid image status")
	}
	return []byte(imageStatusNames[s]), nil
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
	// Unlisted is the files beyond MaxListed in either list.
	Unlisted uint `json:"unlisted,default"`
}

// AllowUnknownJSONFields: ImageCheck has no deny_unknown_fields.
func (ImageCheck) AllowUnknownJSONFields() {}

// MarshalStrictJSON writes serde's form: checked, repaired, damaged and
// unlisted are left out when None, empty or zero (skip_serializing_if).
func (c ImageCheck) MarshalStrictJSON() (Value, error) {
	status, err := c.Status.MarshalText()
	if err != nil {
		return Value{}, err
	}
	members := []Member{{Key: "status", Value: StringValue(string(status))}}
	if c.Checked != nil {
		members = append(members, Member{Key: "checked", Value: StringValue(*c.Checked)})
	}
	list := func(names []string) Value {
		items := make([]Value, len(names))
		for i, name := range names {
			items[i] = StringValue(name)
		}
		return ArrayValue(items...)
	}
	if len(c.Repaired) > 0 {
		members = append(members, Member{Key: "repaired", Value: list(c.Repaired)})
	}
	if len(c.Damaged) > 0 {
		members = append(members, Member{Key: "damaged", Value: list(c.Damaged)})
	}
	if c.Unlisted != 0 {
		members = append(members, Member{Key: "unlisted", Value: U64Value(uint64(c.Unlisted))})
	}
	return ObjectValue(members...), nil
}

// firstChars is Rust's `s.chars().take(n).collect()`.
func firstChars(s string, n int) string {
	count := 0
	for i := range s {
		if count == n {
			return s[:i]
		}
		count++
	}
	return s
}

// ImageCheckFromRecord is what the mixer reports from its record, given
// whether the image has a file list (the SHA-256 of image.sha256, or nil).
// A record written for an older list is still pending.
func ImageCheckFromRecord(record *string, manifestDigest *string) ImageCheck {
	if manifestDigest == nil {
		return ImageCheck{}
	}
	digest := *manifestDigest
	pending := ImageCheck{Status: ImageStatusPending}
	if record == nil {
		return pending
	}
	text := *record
	if len(text) > maxRecord {
		text = text[:maxRecord]
	}
	check := pending
	var status *ImageStatus
	var manifest *string
	// str::lines: split at \n; a \r just before a \n goes with it.
	lines := strings.Split(text, "\n")
	for i, line := range lines {
		if i < len(lines)-1 {
			line = strings.TrimSuffix(line, "\r")
		}
		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		var list *[]string
		switch key {
		case "status":
			status = nil
			switch value {
			case "ok":
				status = ptrTo(ImageStatusOk)
			case "repaired":
				status = ptrTo(ImageStatusRepaired)
			case "damaged":
				status = ptrTo(ImageStatusDamaged)
			}
			continue
		case "manifest":
			manifest = ptrTo(value)
			continue
		case "checked":
			check.Checked = ptrTo(firstChars(value, 64))
			continue
		case "repaired":
			list = &check.Repaired
		case "damaged":
			list = &check.Damaged
		default:
			continue
		}
		if len(*list) < MaxListed {
			*list = append(*list, firstChars(value, 256))
		} else {
			check.Unlisted++
		}
	}
	if status != nil && manifest != nil && *manifest == digest {
		check.Status = *status
		return check
	}
	return pending
}

func ptrTo[T any](v T) *T { return &v }
