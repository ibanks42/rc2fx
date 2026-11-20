package developer

import (
	"errors"
	"runtime"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
)

func status(managed bool, unmanaged uint32, running *bool) core.SSHStatus {
	s := core.SSHStatus{UnmanagedKeys: unmanaged, HostKeys: []string{}, ServerRunning: running}
	if managed {
		fp := "SHA256:abc"
		s.Managed, s.ManagedFingerprint = true, &fp
	}
	return s
}

func boolP(v bool) *bool { return &v }

func reading(s core.SSHStatus) *Reading { return &Reading{Status: &s} }

func flagIs(f *Flag, text string, tone model.Tone) bool {
	return f != nil && f.Text == text && f.Tone == tone
}

func checkAction(t *testing.T, status *Reading, change Change, wantEnable, wantOK bool) {
	t.Helper()
	enable, ok := Action(status, change)
	if ok != wantOK || (ok && enable != wantEnable) {
		t.Fatalf("action: got (%v, %v), want (%v, %v)", enable, ok, wantEnable, wantOK)
	}
}

func TestOldMixerSoftwareSaysUpdateAndIsNeverShownAsOff(t *testing.T) {
	old := &Reading{}
	summary := Describe(old, ChangeNone, "")
	if !strings.Contains(summary.Line, "Update the RC2FX mixer software") {
		t.Fatalf("line %q", summary.Line)
	}
	if !flagIs(summary.Flag, "Update needed", model.ToneWarn) {
		t.Fatalf("flag %+v", summary.Flag)
	}
	checkAction(t, old, ChangeNone, false, false)
}

func TestManagedAndUnmanagedAccessAreBothReported(t *testing.T) {
	s := reading(status(true, 2, boolP(true)))
	on := Describe(s, ChangeNone, "SHA256:abc")
	if !strings.Contains(on.Line, "this computer's key") || !flagIs(on.Flag, "On", model.ToneUpdate) {
		t.Fatalf("%+v", on)
	}
	if !strings.Contains(on.Caution, "2 other keys") {
		t.Fatalf("caution %q", on.Caution)
	}
	checkAction(t, s, ChangeNone, false, true)
	off := reading(status(false, 1, nil))
	summary := Describe(off, ChangeNone, "")
	if !flagIs(summary.Flag, "Off", model.ToneUnknown) {
		t.Fatalf("flag %+v", summary.Flag)
	}
	if !strings.Contains(summary.Caution, "1 other key can") {
		t.Fatalf("caution %q", summary.Caution)
	}
	checkAction(t, off, ChangeNone, true, true)
}

func TestUnknownOrPendingOffersNoAction(t *testing.T) {
	if line := Describe(nil, ChangeNone, "").Line; line != "Checking…" {
		t.Fatalf("line %q", line)
	}
	checkAction(t, nil, ChangeNone, false, false)
	failed := &Reading{Err: errors.New("busy")}
	if !strings.Contains(Describe(failed, ChangeNone, "").Line, "busy") {
		t.Fatal("error not shown")
	}
	checkAction(t, failed, ChangeNone, false, false)
	s := reading(status(false, 0, boolP(true)))
	checkAction(t, s, ChangeEnabling, false, false)
	if !strings.HasPrefix(Describe(s, ChangeEnabling, "").Line, "Enabling") {
		t.Fatal("not enabling")
	}
}

func TestTheLoginCommandQuotesPaths(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Rust: #[cfg(not(windows))]")
	}
	cmd := LoginCommand("/home/a b/.ssh/rc2fx", "/tmp/it's/known_hosts", "rc2fx-0123456789ab", "")
	want := `ssh -i '/home/a b/.ssh/rc2fx' -o UserKnownHostsFile='/tmp/it'\''s/known_hosts' ` +
		`-o HostKeyAlias=rc2fx-0123456789ab root@MIXER_IP`
	if cmd != want {
		t.Fatalf("got  %s\nwant %s", cmd, want)
	}
}

func TestATypedAddressReplacesThePlaceholder(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Rust: #[cfg(not(windows))]")
	}
	cmd := LoginCommand("/k", "/k", "rc2fx-0", "192.168.1.20")
	if !strings.HasSuffix(cmd, "root@192.168.1.20") || strings.Contains(cmd, "MIXER_IP") {
		t.Fatal(cmd)
	}
}

func TestTheAddressFieldAcceptsAddressesAndNothingElse(t *testing.T) {
	for _, ok := range []string{"", "192.168.1.20", "fe80::1", "rodecaster.local", "mixer-1"} {
		if !ValidAddress(ok) {
			t.Fatalf("%q refused", ok)
		}
	}
	for _, bad := range []string{"a b", "1.2.3.4;rm", "$(x)", "a'b", "a\nb", "a\"b", "a`b"} {
		if ValidAddress(bad) {
			t.Fatalf("%q accepted", bad)
		}
	}
	if ValidAddress(strings.Repeat("a", 254)) {
		t.Fatal("254 characters accepted")
	}
}

func TestAReadAnsweredBeforeAChangeIsIgnored(t *testing.T) {
	var ssh SSH
	read := ssh.Track(false)
	change := ssh.Track(true)
	ssh.Change = ChangeEnabling
	if ssh.AcceptReply(read) {
		t.Fatal("stale read")
	}
	if ssh.Change != ChangeEnabling {
		t.Fatal("pending cleared")
	}
	if !ssh.AcceptReply(change) {
		t.Fatal("the change's own reply")
	}
	if ssh.Change != ChangeNone {
		t.Fatal("pending kept")
	}
	read = ssh.Track(false)
	if !ssh.AcceptReply(read) {
		t.Fatal("read refused")
	}
}

func TestRepliesFromBeforeAClearAreIgnored(t *testing.T) {
	var ssh SSH
	old := ssh.Track(true)
	ssh.Clear()
	fresh := ssh.Track(false)
	if old == fresh {
		t.Fatal("ids are never reused")
	}
	if ssh.AcceptReply(old) {
		t.Fatal("old reply accepted")
	}
	if len(ssh.sent) != 1 {
		t.Fatal("a stale reply consumes nothing")
	}
	if !ssh.AcceptReply(fresh) {
		t.Fatal("new reply refused")
	}
	if ssh.AcceptReply(fresh) {
		t.Fatal("each reply counts once")
	}
}

func TestAnotherComputersKeyIsNamedAndGetsNoLoginCommand(t *testing.T) {
	s := status(true, 0, boolP(true))
	st := reading(s)
	if line := Describe(st, ChangeNone, "SHA256:mine").Line; !strings.Contains(line, "another computer") {
		t.Fatalf("line %q", line)
	}
	if UsesLocalKey(&s, "SHA256:mine") || UsesLocalKey(&s, "") || !UsesLocalKey(&s, "SHA256:abc") {
		t.Fatal("uses local key")
	}
	if line := Describe(st, ChangeNone, "").Line; !strings.HasPrefix(line, "Logins use an RC2FX-managed key") {
		t.Fatalf("line %q", line)
	}
}

func TestClearingAlwaysMovesTheEpoch(t *testing.T) {
	var ssh SSH
	before := ssh.Epoch
	ssh.Change = ChangeEnabling
	ssh.Track(true)
	ssh.Clear()
	if ssh.Epoch == before {
		t.Fatal("epoch unchanged")
	}
	if ssh.Change != ChangeNone || len(ssh.sent) != 0 {
		t.Fatal("not cleared")
	}
}

func TestAStoppedServerIsMentioned(t *testing.T) {
	summary := Describe(reading(status(true, 0, boolP(false))), ChangeNone, "")
	if !strings.Contains(summary.Line, "not running") {
		t.Fatalf("line %q", summary.Line)
	}
	if !flagIs(summary.Flag, "Not running", model.ToneWarn) {
		t.Fatalf("flag %+v", summary.Flag)
	}
}

func TestPreparingTheKeyAlreadyCountsAsEnabling(t *testing.T) {
	s := reading(status(false, 0, boolP(true)))
	if !strings.HasPrefix(Describe(s, ChangePreparingKey, "").Line, "Enabling") {
		t.Fatal("not enabling")
	}
	checkAction(t, s, ChangePreparingKey, false, false)
	if !ChangePreparingKey.Busy() || ChangePreparingKey.Sent() {
		t.Fatal("preparing the key is busy but nothing was sent")
	}
	for _, c := range []Change{ChangeEnabling, ChangeDisabling} {
		if !c.Busy() || !c.Sent() {
			t.Fatalf("%v is sent", c)
		}
	}
	if ChangeNone.Busy() || ChangeNone.Sent() {
		t.Fatal("nothing is changing")
	}
	if !strings.HasPrefix(Describe(reading(status(true, 0, boolP(true))), ChangeDisabling, "").Line, "Disabling") {
		t.Fatal("not disabling")
	}
}
