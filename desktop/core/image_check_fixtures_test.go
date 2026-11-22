package core

// Inline desktop-side decoding contract tests for GET api/v1/image-check.
// Both Rust and Go share ImageCheck / ImageStatus generated from schema/core.go.

import (
	"reflect"
	"testing"
)

func TestImageCheckReportContract(t *testing.T) {
	cases := []struct {
		name      string
		input     string
		accept    bool
		canonical string
	}{
		{"api-example", `{"status": "repaired", "checked": "2026-11-15T22:10:00Z", "repaired": ["usr/bin/rode-init-update-env.sh"]}`, true, `{"status":"repaired","checked":"2026-11-15T22:10:00Z","repaired":["usr/bin/rode-init-update-env.sh"]}`},
		{"newer-fields", `{"status":"ok","later":1}`, true, `{"status":"ok"}`},
		{"everything", `{"status":"damaged","checked":"2026-11-15T22:10:00Z","repaired":[],"damaged":["etc/hostname"],"unlisted":3}`, true, `{"status":"damaged","checked":"2026-11-15T22:10:00Z","damaged":["etc/hostname"],"unlisted":3}`},
		{"empty", `{}`, true, `{"status":"none"}`},
		{"null-checked", `{"status":"pending","checked":null}`, true, `{"status":"pending"}`},
		{"each-status", `{"status":"ok","unlisted":0}`, true, `{"status":"ok"}`},
		{"unknown-status", `{"status":"bogus"}`, false, ""},
		{"status-case", `{"status":"Ok"}`, false, ""},
		{"null-status", `{"status":null}`, false, ""},
		{"negative-unlisted", `{"status":"ok","unlisted":-1}`, false, ""},
		{"fractional-unlisted", `{"status":"ok","unlisted":1.5}`, false, ""},
		{"null-list", `{"status":"ok","repaired":null}`, false, ""},
		{"number-in-list", `{"status":"damaged","damaged":[1]}`, false, ""},
		{"not-an-object", `"ok"`, false, ""},
	}
	for _, c := range cases {
		var check ImageCheck
		err := DecodeStrict([]byte(c.input), &check)
		if (err == nil) != c.accept {
			t.Errorf("%s: accepted %t, want %t (%v)", c.name, err == nil, c.accept, err)
			continue
		}
		if !c.accept {
			continue
		}
		var canonical ImageCheck
		if err := DecodeStrict([]byte(c.canonical), &canonical); err != nil {
			t.Fatalf("%s: canonical: %v", c.name, err)
		}
		if !reflect.DeepEqual(normalImageCheck(check), normalImageCheck(canonical)) {
			t.Errorf("%s:\n got %+v\nwant %+v", c.name, check, canonical)
		}
	}
}

func normalImageCheck(c ImageCheck) ImageCheck {
	if len(c.Repaired) == 0 {
		c.Repaired = nil
	}
	if len(c.Damaged) == 0 {
		c.Damaged = nil
	}
	return c
}
