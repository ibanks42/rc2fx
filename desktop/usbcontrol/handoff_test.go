package usbcontrol

import "testing"

func resetHandoff() {
	handoff.mu.Lock()
	handoff.latest, handoff.resume = nil, nil
	handoff.mu.Unlock()
}

func TestARestartedAppContinuesTheSessionPastTheOldRequests(t *testing.T) {
	resetHandoff()
	t.Cleanup(resetHandoff)
	if _, ok := HandoffToken(); ok {
		t.Fatal("token without a session")
	}
	c := &Client{serial: "SER1", nonce: 0xabc, sequence: 41}
	c.remember()
	token, ok := HandoffToken()
	if !ok || token != "SER1:abc:10029" {
		t.Fatalf("%q %v", token, ok)
	}
	if !ResumeFrom(token) {
		t.Fatal("refused its own token")
	}
	got := takeResume()
	if got == nil || *got != (sessionToken{serial: "SER1", nonce: 0xabc, sequence: 41 + handoffGap}) {
		t.Fatalf("%+v", got)
	}
	if takeResume() != nil {
		t.Fatal("a token seeds one Open only")
	}
}

func TestBadTokensAreIgnored(t *testing.T) {
	resetHandoff()
	t.Cleanup(resetHandoff)
	for _, token := range []string{"", "SER1", "SER1:abc", ":abc:1", "SER1:0:1", "SER1:zz:1", "SER1:abc:1ffffffff", "a:b:c:d"} {
		if ResumeFrom(token) {
			t.Errorf("%q accepted", token)
		}
	}
	if takeResume() != nil {
		t.Fatal("seeded")
	}
	// Near exhaustion there is no room for the gap.
	(&Client{serial: "SER1", nonce: 1, sequence: ^uint32(0) - handoffGap}).remember()
	if _, ok := HandoffToken(); ok {
		t.Fatal("token near exhaustion")
	}
}
