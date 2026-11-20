package model

import "testing"

func TestNamesAreTitleCaseWithUSBInCapitals(t *testing.T) {
	for _, c := range [][2]string{
		{"USB a", "USB A"},
		{"usb main", "USB Main"},
		{"Main Mix Out", "Main Mix Out"},
		{"CallMe 1", "CallMe 1"},
		{"headphones 2", "Headphones 2"},
		{"SMART pads", "SMART Pads"},
	} {
		if got := DisplayName(c[0]); got != c[1] {
			t.Fatalf("DisplayName(%q) = %q, want %q", c[0], got, c[1])
		}
	}
}
