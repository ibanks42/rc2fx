//go:build linux

package archive

import (
	"reflect"
	"testing"
)

func TestAPrivilegedHelperReadsOnlyItsInvokersFiles(t *testing.T) {
	u := func(v uint32) *uint32 { return &v }
	if !invokerOwns(1000, u(1000)) {
		t.Fatal("the invoker's own file")
	}
	if invokerOwns(0, u(1000)) {
		t.Fatal("a root file placed in the folder")
	}
	if invokerOwns(1001, u(1000)) {
		t.Fatal("another user's file")
	}
	if !invokerOwns(0, nil) || invokerOwns(1000, nil) {
		t.Fatal("root's own folder")
	}
}

func TestTheInvokerComesFromPkexecOrASudoUserTheSystemKnows(t *testing.T) {
	found := func(vars map[string]string) (*[2]uint32, error) {
		return invoker(
			func(name string) (string, bool) { v, ok := vars[name]; return v, ok },
			func(uid uint32) (uint32, bool) { return 100, uid == 1000 },
			func(name string) (uint32, bool) { return 1000, name == "me" },
		)
	}
	if got, err := found(nil); err != nil || got != nil {
		t.Fatalf("%v %v", got, err)
	}
	// pkexec wins over anything claimed through sudo's variables.
	both := map[string]string{"PKEXEC_UID": "1000", "SUDO_UID": "1", "SUDO_GID": "1", "SUDO_USER": "someone"}
	if got, err := found(both); err != nil || !reflect.DeepEqual(got, &[2]uint32{1000, 100}) {
		t.Fatalf("%v %v", got, err)
	}
	sudo := func(uid, user string) map[string]string {
		return map[string]string{"SUDO_UID": uid, "SUDO_GID": "100", "SUDO_USER": user}
	}
	if got, err := found(sudo("1000", "me")); err != nil || !reflect.DeepEqual(got, &[2]uint32{1000, 100}) {
		t.Fatalf("%v %v", got, err)
	}
	for _, bad := range []map[string]string{
		sudo("1", "me"),        // uid is not that user's
		sudo("1000", "nobody"), // unknown user
		{"SUDO_UID": "1000", "SUDO_GID": "100"},
		{"PKEXEC_UID": "2000"}, // unknown uid
	} {
		if _, err := found(bad); err == nil {
			t.Fatalf("%v accepted", bad)
		}
	}
}
