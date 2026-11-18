package transport

// The app's USB session: one goroutine owns the HID handle and serves
// requests in order. After a broken request, queued requests are refused and
// only a fresh read reopens the session, so a change is never sent on a state
// the app has not re-read, and none is replayed.

import (
	"context"
	"encoding/json"
	"errors"
	"runtime"
	"sync"

	"github.com/ibanks42/rc2fx/desktop/installer/control"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

type outcome struct {
	reply usbproto.Reply
	err   error
}

type job struct {
	// Done when the caller stopped waiting, as a dropped oneshot receiver is.
	ctx     context.Context
	request usbproto.Request
	answer  chan<- outcome
}

// Session is something that serves requests: the real client, or a test
// stand-in. Request returns an *control.Failure when it fails.
type Session interface {
	Request(request *usbproto.Request) (usbproto.Reply, error)
	IsOpen() bool
	Reopen() error
}

// usbLink is what every holder of one Usb shares. The serving goroutine
// stops once the last holder lets go.
type usbLink struct {
	jobs    chan job
	stopped chan struct{}
	mu      sync.Mutex
	holders int
}

// Usb is one holder's handle on the session. Each holder releases its own.
type Usb struct {
	link *usbLink
}

func (u Usb) clone() Usb {
	u.link.mu.Lock()
	u.link.holders++
	u.link.mu.Unlock()
	return u
}

func (u Usb) release() {
	u.link.mu.Lock()
	defer u.link.mu.Unlock()
	u.link.holders--
	if u.link.holders == 0 {
		close(u.link.jobs)
	}
}

type opened struct {
	serial string
	err    error
}

// connectUSB opens the session; also returns the mixer's USB serial.
func connectUSB(ctx context.Context) (Usb, string, error) {
	link := &usbLink{
		jobs:    make(chan job, 8),
		stopped: make(chan struct{}),
		holders: 1,
	}
	ready := make(chan opened, 1)
	go func() {
		// HID handles stay on the thread that opened them.
		runtime.LockOSThread()
		defer close(link.stopped)
		session, serial, err := openSession()
		if err != nil {
			ready <- opened{err: err}
			return
		}
		ready <- opened{serial: serial}
		serve(link.jobs, session)
		if closer, ok := session.(interface{ Close() }); ok {
			closer.Close()
		}
	}()
	select {
	case result := <-ready:
		if result.err != nil {
			return Usb{}, "", result.err
		}
		return Usb{link}, result.serial, nil
	case <-ctx.Done():
		// Nobody will hold a session that opens after this.
		go func() {
			if result := <-ready; result.err == nil {
				Usb{link}.release()
			}
		}()
		return Usb{}, "", ctx.Err()
	}
}

func (u Usb) request(ctx context.Context, method, path string, body json.RawMessage) (usbproto.Reply, *requestError) {
	if body != nil {
		compacted, err := compact(body)
		if err != nil || len(compacted) > requestLimit {
			return usbproto.Reply{}, errTooLarge()
		}
		body = compacted
	}
	answer := make(chan outcome, 1)
	request := usbproto.Request{
		Method: method,
		Path:   path,
		Body:   body,
	}
	select {
	case u.link.jobs <- job{ctx: ctx, request: request, answer: answer}:
	case <-u.link.stopped:
		return usbproto.Reply{}, usbError("USB disconnected")
	case <-ctx.Done():
		return usbproto.Reply{}, usbError(ctx.Err().Error())
	}
	select {
	case result := <-answer:
		if result.err != nil {
			return usbproto.Reply{}, usbError(result.err.Error())
		}
		return result.reply, nil
	case <-u.link.stopped:
		// It may have answered just before stopping.
		select {
		case result := <-answer:
			if result.err != nil {
				return usbproto.Reply{}, usbError(result.err.Error())
			}
			return result.reply, nil
		default:
			return usbproto.Reply{}, usbError("USB worker stopped")
		}
	case <-ctx.Done():
		return usbproto.Reply{}, usbError(ctx.Err().Error())
	}
}

func refuse(answer chan<- outcome, message string) {
	answer <- outcome{err: errors.New(message)}
}

// failureMessage is the text a failed request answers with.
func failureMessage(err error) string {
	var failure *control.Failure
	if errors.As(err, &failure) {
		return failure.Message
	}
	return err.Error()
}

// broken is whether the session broke and the request's outcome is unknown.
func broken(err error) bool {
	var failure *control.Failure
	return errors.As(err, &failure) && failure.Kind == control.Broken
}

func serve(jobs <-chan job, session Session) {
	for job := range jobs {
		if job.ctx.Err() != nil {
			continue
		}
		if !session.IsOpen() {
			if job.request.Method != "GET" || job.request.Body != nil {
				refuse(job.answer, "USB reconnected; wait for fresh state before sending changes")
				continue
			}
			if err := session.Reopen(); err != nil {
				refuse(job.answer, err.Error())
				continue
			}
		}
		reply, err := session.Request(&job.request)
		switch {
		case err == nil:
			job.answer <- outcome{reply: reply}
		case broken(err):
			refuse(job.answer, failureMessage(err))
			for drained := false; !drained; {
				select {
				case queued, ok := <-jobs:
					if !ok {
						return
					}
					refuse(queued.answer, "USB disconnected; queued request was not sent")
				default:
					drained = true
				}
			}
		default:
			refuse(job.answer, failureMessage(err))
		}
	}
}
