package transport

import (
	"context"
	"encoding/json"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// flaky breaks on its first request, then serves reads after reopening.
type flaky struct {
	open       bool
	brokenOnce bool
	reopens    *atomic.Int64
}

func (f *flaky) Request(*usbproto.Request) (usbproto.Reply, error) {
	if !f.brokenOnce {
		f.brokenOnce = true
		f.open = false
		return usbproto.Reply{}, &usbcontrol.Failure{Kind: usbcontrol.Broken, Message: "lost"}
	}
	return usbproto.Reply{Status: 200, Body: json.RawMessage("null")}, nil
}

func (f *flaky) IsOpen() bool {
	return f.open
}

func (f *flaky) Reopen() error {
	f.reopens.Add(1)
	f.open = true
	return nil
}

func testJob(method string, body json.RawMessage) (job, <-chan outcome) {
	answer := make(chan outcome, 1)
	request := usbproto.Request{
		Method: method,
		Path:   "api/v1/config",
		Body:   body,
	}
	return job{ctx: context.Background(), request: request, answer: answer}, answer
}

func refusedWith(t *testing.T, reply <-chan outcome, text string) {
	t.Helper()
	result := <-reply
	if result.err == nil || !strings.Contains(result.err.Error(), text) {
		t.Fatalf("want an error containing %q, got %v", text, result.err)
	}
}

func TestAfterABrokenRequestQueuedOnesAreRefusedAndOnlyAReadReopens(t *testing.T) {
	jobs := make(chan job, 8)
	reopens := &atomic.Int64{}
	first, firstReply := testJob("PUT", json.RawMessage("{}"))
	queued, queuedReply := testJob("PUT", json.RawMessage("{}"))
	jobs <- first
	jobs <- queued
	change, changeReply := testJob("PUT", json.RawMessage("{}"))
	read, readReply := testJob("GET", nil)
	jobs <- change
	jobs <- read
	close(jobs)
	session := &flaky{open: true, brokenOnce: false, reopens: reopens}
	serve(jobs, session)
	if result := <-firstReply; result.err == nil {
		t.Fatal("the broken request succeeded")
	}
	refusedWith(t, queuedReply, "not sent")
	// Queued with the failure, so also refused unsent.
	refusedWith(t, changeReply, "not sent")
	refusedWith(t, readReply, "not sent")
	if got := reopens.Load(); got != 0 {
		t.Fatalf("reopened %d times", got)
	}
}

func TestAChangeWaitsForAFreshReadAfterReconnecting(t *testing.T) {
	jobs := make(chan job, 8)
	reopens := &atomic.Int64{}
	session := &flaky{open: false, brokenOnce: true, reopens: reopens}
	change, changeReply := testJob("PUT", json.RawMessage("{}"))
	read, readReply := testJob("GET", nil)
	jobs <- change
	jobs <- read
	close(jobs)
	serve(jobs, session)
	refusedWith(t, changeReply, "fresh state")
	result := <-readReply
	if result.err != nil || result.reply.Status != 200 {
		t.Fatalf("the read got %v, %v", result.reply.Status, result.err)
	}
	if got := reopens.Load(); got != 1 {
		t.Fatalf("reopened %d times", got)
	}
}
