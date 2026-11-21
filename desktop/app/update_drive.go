package app

// Desktop side of the mixer's temporary update drive. When possible, rc2fxd
// attaches a short-lived FAT volume named RC2FX to RØDE's removable USB drive
// so in-place updates copy over mass storage instead of streaming over USB 1
// (see transport.DriveWriter).

import (
	"context"
	"errors"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

// updateDriveLook is the polling interval while waiting for the RC2FX drive to appear.
const updateDriveLook = 250 * time.Millisecond

// updateDriveWriter returns a DriveWriter that waits for the mixer's temporary
// update drive, copies the payload onto it, and ejects it. Returns nil if the
// mixer's serial number isn't known or this platform can't write to removable
// mixer disks, in which case the upload falls back to USB 1.
func updateDriveWriter(serial *string) transport.DriveWriter {
	p := platform.Host()
	if serial == nil || *serial == "" || !flash.CanReport(p) {
		return nil
	}
	mixer := *serial
	return func(ctx context.Context, label, name string, data []byte) error {
		var v platform.Volume
		for {
			found, ok, err := flash.ReportDisk(p, mixer)
			if err == nil && ok {
				v = found
				break
			}
			select {
			case <-ctx.Done():
				return errors.New("the mixer's update drive did not appear")
			case <-time.After(updateDriveLook):
			}
		}
		if err := flash.WriteReport(p, v, name, data); err != nil {
			return err
		}
		// Best-effort eject. If the OS still holds the volume, the mixer tries to
		// detach it on its own; if that fails too, the mixer rejects the drive and
		// the transport falls back to USB 1.
		_ = p.Eject(&v)
		return nil
	}
}
