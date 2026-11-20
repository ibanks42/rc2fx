package state

import (
	"errors"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// AccessOf maps a privilege grant error (or nil) to an installflow.Access state.
func AccessOf(err error) installflow.Access {
	switch {
	case err == nil:
		return installflow.Access{Kind: installflow.AccessGranted}
	case errors.Is(err, helper.ErrNotGranted):
		return installflow.Access{
			Kind: installflow.AccessRefused,
			Why:  "The password prompt was closed, or the password wasn't accepted.",
		}
	}
	return installflow.Access{Kind: installflow.AccessRefused, Why: err.Error()}
}
