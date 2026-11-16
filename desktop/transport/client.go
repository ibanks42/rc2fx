//go:build !mockmixer

package transport

import (
	"github.com/ibanks42/rc2fx/desktop/installer/control"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// clientSession serves requests through the installer's control client.
type clientSession struct {
	client *control.Client
}

func (s clientSession) Request(request *usbproto.Request) (usbproto.Reply, error) {
	reply, err := s.client.Request(request)
	if err != nil {
		return usbproto.Reply{}, err
	}
	return *reply, nil
}

func (s clientSession) IsOpen() bool {
	return s.client.IsOpen()
}

func (s clientSession) Reopen() error {
	return s.client.Reopen()
}

// Close closes the HID handle once the session is no longer served.
func (s clientSession) Close() {
	s.client.Close()
}

// openSession opens the one connected mixer; called on the goroutine that
// will serve it.
func openSession() (Session, string, error) {
	client, err := control.Open()
	if err != nil {
		return nil, "", err
	}
	return clientSession{client}, client.Serial(), nil
}
