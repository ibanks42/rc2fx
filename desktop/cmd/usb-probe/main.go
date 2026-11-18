// Command usb-probe is an end-to-end USB check. Read-only unless
// --write-roundtrip is explicitly used; that flag reapplies the exact current
// targets with optimistic revision checking.
package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func main() {
	if err := probe(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %q\n", err.Error())
		os.Exit(1)
	}
}

// lookup is value[key][key]..., or null where something is missing.
func lookup(value json.RawMessage, keys ...string) json.RawMessage {
	for _, key := range keys {
		var fields map[string]json.RawMessage
		if json.Unmarshal(value, &fields) != nil || fields == nil {
			return json.RawMessage("null")
		}
		next, ok := fields[key]
		if !ok {
			return json.RawMessage("null")
		}
		value = next
	}
	return value
}

// display is a value as compact JSON, or as it came when it does not parse.
func display(value json.RawMessage) string {
	var out bytes.Buffer
	if json.Compact(&out, value) != nil {
		return string(value)
	}
	return out.String()
}

// asUint is a plain unsigned integer: digits only, within 64 bits.
func asUint(value json.RawMessage) (uint64, bool) {
	text := string(bytes.TrimSpace(value))
	if text == "" {
		return 0, false
	}
	for _, c := range text {
		if c < '0' || c > '9' {
			return 0, false
		}
	}
	n, err := strconv.ParseUint(text, 10, 64)
	return n, err == nil
}

// equal is Value equality: the same tree, whatever the key order.
func equal(a, b json.RawMessage) bool {
	decode := func(raw json.RawMessage) (any, bool) {
		decoder := json.NewDecoder(bytes.NewReader(raw))
		decoder.UseNumber()
		var value any
		return value, decoder.Decode(&value) == nil
	}
	left, okLeft := decode(a)
	right, okRight := decode(b)
	return okLeft && okRight && reflect.DeepEqual(left, right)
}

func probe(args []string) error {
	writeRoundtrip := slices.Contains(args, "--write-roundtrip")
	extended := slices.Contains(args, "--extended")
	seconds := uint64(10)
	for i := 0; i+1 < len(args); i++ {
		if args[i] == "--seconds" {
			parsed, err := strconv.ParseUint(strings.TrimPrefix(args[i+1], "+"), 10, 64)
			if err != nil {
				return errors.New("Invalid --seconds")
			}
			seconds = parsed
			break
		}
	}
	seconds = min(max(seconds, 10), 3600)
	t, err := transport.New()
	if err != nil {
		return err
	}
	defer t.Close()
	if err := t.Send(transport.CommandConnect{Generation: 1}); err != nil {
		return err
	}
	deadline := time.Now().Add(70 * time.Second)
	connected := false
	stream := false
	stateEvents := 0
	meterEvents := 0
	usbEvents := 0
	applied := !writeRoundtrip
	var expectedTargets json.RawMessage
	reads := 0
	var firstMeter, lastMeter json.RawMessage
	finish := deadline
	for time.Now().Before(finish) {
		for {
			event, ok := t.TryRecv()
			if !ok {
				break
			}
			switch kind := event.Kind.(type) {
			case transport.EventKindConnected:
				state := kind.State
				fmt.Printf("USB connected; settings revision %s\n", display(lookup(state, "config", "revision")))
				connected = true
				finish = time.Now().Add(time.Duration(seconds) * time.Second)
				if writeRoundtrip {
					targets := lookup(state, "config", "targets")
					expectedTargets = targets
					revision, ok := asUint(lookup(state, "config", "revision"))
					if !ok {
						return errors.New("Missing revision")
					}
					var decoded []core.Target
					if err := core.DecodeStrict(targets, &decoded); err != nil {
						return errors.New(err.Error())
					}
					if err := t.Send(transport.CommandApply{
						Generation: 1,
						Update: core.ConfigUpdate{
							ExpectedRevision: revision,
							Targets:          decoded,
						},
					}); err != nil {
						return err
					}
				}
				if extended {
					if err := t.Send(transport.CommandInputs{Generation: 1}); err != nil {
						return err
					}
					if err := t.Send(transport.CommandPreset{Generation: 1, Action: transport.PresetActionList{}}); err != nil {
						return err
					}
					if err := t.Send(transport.CommandDiagnostics{Generation: 1}); err != nil {
						return err
					}
				}
			case transport.EventKindStreamStatus:
				fmt.Printf("USB stream: %s\n", kind.Message)
				stream = kind.Connected
			case transport.EventKindState:
				stateEvents++
			case transport.EventKindMeter:
				meterEvents++
				if firstMeter == nil {
					firstMeter = kind.Meter
				}
				lastMeter = kind.Meter
			case transport.EventKindUSB:
				usbEvents++
			case transport.EventKindApplied:
				if expectedTargets == nil || !equal(expectedTargets, lookup(kind.State, "config", "targets")) {
					return errors.New("Applied targets differ from the unchanged input")
				}
				fmt.Printf("Unchanged settings round trip applied; revision %s\n", display(lookup(kind.State, "config", "revision")))
				applied = true
			case transport.EventKindConflict:
				return errors.New("Settings changed concurrently; no write retried")
			case transport.EventKindInputs:
				if kind.Err != nil {
					return kind.Err
				}
				reads++
				fmt.Println("USB input inventory received")
			case transport.EventKindPreset:
				if kind.Err != nil {
					return errors.New(kind.Err.Message)
				}
				reads++
				fmt.Println("USB preset inventory received")
			case transport.EventKindDiagnostics:
				if kind.Err != nil {
					return kind.Err
				}
				reads++
				fmt.Printf("USB diagnostics received (%d bytes)\n", len(display(kind.Report)))
			case transport.EventKindFailure:
				return errors.New(kind.Message)
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	fmt.Printf("state events: %d; meter events: %d; USB observations: %d\n", stateEvents, meterEvents, usbEvents)
	if !connected || !stream || stateEvents == 0 || !applied || (extended && reads != 3) {
		return errors.New("USB state/event round trip did not complete")
	}
	if firstMeter != nil && lastMeter != nil {
		fmt.Printf("First meter: %s\n", display(firstMeter))
		fmt.Printf("Last meter: %s\n", display(lastMeter))
	}
	return nil
}
