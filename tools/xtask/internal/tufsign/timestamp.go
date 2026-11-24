package tufsign

import (
	"time"
)

// FormatExpires formats t in UTC with 0, 3, 6, or 9 fractional digits
// (matching tough's serialize_timestamp so re-serialization reproduces signed
// bytes, including trailing zeros within the chosen group).
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

// DisplayTimestamp formats t in UTC RFC 3339 without trailing fractional zeros.
func DisplayTimestamp(t time.Time) string {
	return t.UTC().Format(time.RFC3339Nano)
}

// ParseTimestamp parses an RFC 3339 timestamp and normalizes it to UTC.
func ParseTimestamp(text string) (time.Time, error) {
	t, err := time.Parse(time.RFC3339Nano, text)
	if err != nil {
		return time.Time{}, err
	}
	return t.UTC(), nil
}
