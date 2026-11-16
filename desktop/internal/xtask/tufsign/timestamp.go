package tufsign

import (
	"time"
)

// FormatExpires is tough's serialize_timestamp: UTC, with 0, 3, 6 or 9
// fractional second digits, the fewest that represent the value exactly.
// Re-serializing must reproduce the signed bytes, so trailing zeros inside
// the chosen group are kept.
func FormatExpires(t time.Time) string {
	t = t.UTC()
	layout := "2006-01-02T15:04:05"
	switch ns := t.Nanosecond(); {
	case ns == 0:
	case ns%1_000_000 == 0:
		layout += ".000"
	case ns%1_000 == 0:
		layout += ".000000"
	default:
		layout += ".000000000"
	}
	return t.Format(layout) + "Z"
}

// DisplayTimestamp is jiff's Display for a Timestamp: UTC, `Z`, and as many
// fractional digits as are not trailing zeros.
func DisplayTimestamp(t time.Time) string {
	return t.UTC().Format(time.RFC3339Nano)
}

// ParseTimestamp reads an RFC 3339 timestamp with an offset, as jiff's
// Timestamp parser does for these files and arguments.
func ParseTimestamp(text string) (time.Time, error) {
	t, err := time.Parse(time.RFC3339Nano, text)
	if err != nil {
		return time.Time{}, err
	}
	return t.UTC(), nil
}
