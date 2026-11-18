package app

import (
	"testing"

	"gpuikit.dev/gk"
)

// A second question while one is open is refused, so the first keeps its
// onClose and the state its caller clears there.
func TestAnOpenConfirmRefusesASecondAndKeepsItsOwnClose(t *testing.T) {
	a := &App{}
	closed := ""
	if !a.claimConfirm(confirmKindRestart, Confirm{Title: "restart"}, nil, func(*gk.Cx) { closed = "restart" }) {
		t.Fatal("the first question was refused")
	}
	if !a.restartAsked() || a.closingPrompt() {
		t.Fatal("the flags do not follow the open question")
	}
	if a.claimConfirm(confirmKindClose, Confirm{Title: "close"}, nil, func(*gk.Cx) { closed = "close" }) {
		t.Fatal("a second question replaced the open one")
	}
	if a.closingPrompt() || !a.restartAsked() {
		t.Fatal("the refused question changed the flags")
	}
	held := a.confirmOpen
	if held == nil || held.spec.Title != "restart" {
		t.Fatalf("the open question is %+v", held)
	}
	held.onClose(nil)
	if closed != "restart" {
		t.Fatalf("the open question's onClose is %q's", closed)
	}
	if !a.releaseConfirm(held) {
		t.Fatal("closing the open question did not free the slot")
	}
	if a.restartAsked() || a.confirmOpen != nil {
		t.Fatal("the restart question is still marked open after it closed")
	}
	if !a.claimConfirm(confirmKindClose, Confirm{Title: "close"}, nil, nil) || !a.closingPrompt() {
		t.Fatal("a question after the first closed was refused")
	}
}

// A dialog that no longer holds the slot does not free it for another.
func TestAStaleCloseLeavesTheOpenConfirm(t *testing.T) {
	a := &App{}
	a.claimConfirm(confirmKindClose, Confirm{}, nil, nil)
	first := a.confirmOpen
	a.releaseConfirm(first)
	a.claimConfirm(confirmKindClose, Confirm{}, nil, nil)
	if a.releaseConfirm(first) {
		t.Fatal("a stale dialog freed the slot")
	}
	if a.releaseConfirm(&confirmState{}) || a.releaseConfirm(nil) {
		t.Fatal("a dialog drawn with nothing open freed the slot")
	}
	if !a.closingPrompt() {
		t.Fatal("the close question is no longer marked open")
	}
}
