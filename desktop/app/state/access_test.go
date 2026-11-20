package state

import (
	"errors"
	"fmt"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

func TestADismissedPromptIsRefusedByOutcomeNotByText(t *testing.T) {
	if got := AccessOf(nil); got.Kind != installflow.AccessGranted {
		t.Fatalf("nil: %+v", got)
	}
	dismissed := "The password prompt was closed, or the password wasn't accepted."
	if got := AccessOf(fmt.Errorf("probe: %w", helper.ErrNotGranted)); got.Kind != installflow.AccessRefused || got.Why != dismissed {
		t.Fatalf("not granted: %+v", got)
	}
	// A reason that happens to contain the words is shown as it is.
	other := errors.New("the mixer said: access not granted to /dev/sda")
	if got := AccessOf(other); got.Kind != installflow.AccessRefused || got.Why != other.Error() {
		t.Fatalf("other: %+v", got)
	}
	if got := AccessOf(helper.ErrStopped); got.Why != helper.ErrStopped.Error() {
		t.Fatalf("stopped: %+v", got)
	}
}
