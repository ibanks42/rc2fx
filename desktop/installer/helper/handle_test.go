package helper

import (
	"bufio"
	"errors"
	"os"
	"os/exec"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

const stubEnv = "RC2FX_HELPER_STUB"

func TestMain(m *testing.M) {
	switch os.Getenv(stubEnv) {
	case "":
		os.Exit(m.Run())
	case "refused":
		// Simulates pkexec when the user dismisses the auth prompt.
		os.Exit(126)
	case "crashed":
		os.Exit(1)
	case "serve":
		lines := bufio.NewReader(os.Stdin)
		if _, err := lines.ReadString('\n'); err != nil {
			os.Exit(2)
		}
		writeLine(os.Stdout, message{Progress: &installer.Progress{Phase: "download", Completed: 1, Total: 2}})
		writeLine(os.Stdout, message{Reply: &installer.Reply{Kind: installer.ReplyDone}})
		// Then wait for stdin to close, as Serve does.
		lines.ReadString('\n')
		os.Exit(0)
	}
	os.Exit(3)
}

func stub(t *testing.T, mode string) *Helper {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe)
	cmd.Env = append(os.Environ(), stubEnv+"="+mode)
	h, err := start(cmd, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { h.Close() })
	return h
}

func probe(h *Helper) error {
	_, err := h.Handle(installer.Request{Kind: installer.RequestReleases}, func(installer.Progress) {})
	return err
}

func TestADismissedPromptIsAlwaysNotGranted(t *testing.T) {
	// The outcome must not depend on whether the read, the write or
	// cmd.Wait sees the exit first.
	for range 20 {
		h := stub(t, "refused")
		if err := probe(h); !errors.Is(err, ErrNotGranted) {
			t.Fatalf("got %v, want ErrNotGranted", err)
		}
		if h.Alive() {
			t.Fatal("a helper that refused is still reported alive")
		}
	}
}

func TestAWriteToAnExitedPromptIsNotGranted(t *testing.T) {
	h := stub(t, "refused")
	<-h.exited
	if err := probe(h); !errors.Is(err, ErrNotGranted) {
		t.Fatalf("got %v, want ErrNotGranted", err)
	}
}

func TestAHelperThatExitsOtherwiseHasStopped(t *testing.T) {
	h := stub(t, "crashed")
	if err := probe(h); !errors.Is(err, ErrStopped) || errors.Is(err, ErrNotGranted) {
		t.Fatalf("got %v, want ErrStopped", err)
	}
	if h.Alive() {
		t.Fatal("a helper that exited is still reported alive")
	}
	if err := probe(h); !errors.Is(err, ErrStopped) {
		t.Fatalf("a second request got %v, want ErrStopped", err)
	}
}

func TestARunningHelperReplies(t *testing.T) {
	h := stub(t, "serve")
	var seen []installer.Progress
	reply, err := h.Handle(installer.Request{Kind: installer.RequestReleases}, func(p installer.Progress) { seen = append(seen, p) })
	if err != nil {
		t.Fatal(err)
	}
	if reply.Kind != installer.ReplyDone || len(seen) != 1 || seen[0].Completed != 1 {
		t.Fatalf("reply %+v, progress %+v", reply, seen)
	}
	if !h.Alive() {
		t.Fatal("a serving helper is reported dead")
	}
}
