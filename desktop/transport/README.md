# transport

RC2FX control over USB 1 from the desktop app (package `transport` in
`desktop/`). It runs inside the app process, starts no listener or external
program, and uses no network.

`CommandConnect` opens the single RØDECaster Pro II on USB 1 through the
installer's control client (`installer/control`). The framing is described in
[USB control](../../docs/reference/usb-control.md). One goroutine holds the
HID handle and serves requests in order.

## Request handling

- A request whose outcome is unknown is not resent. After a broken request,
  queued requests are refused without being sent, and the session reopens only
  after a fresh read. Changes are always based on state the app has re-read.
- Reopening keeps the session's nonce and request numbers, so the mixer treats
  it as the same client. A mixer with a different serial is refused.
- A lost reply is fetched again. If the final acknowledgement is lost, the
  reply the mixer cached is recovered.
- Events are polled with `GET api/v1/events`. Polling backs off while it fails,
  and the backoff resets after a period of successful polls.
- An in-place update (`CommandMixerUpdate`) uploads beside other requests,
  one at a time; a second is refused while one is still sending. If the USB
  session breaks mid-upload, the upload reads the update session to reopen it
  and resumes where the mixer says it is, for up to 30 s.
- The event poll and an in-place update belong to their connection.
  `CommandConnect` and `CommandDisconnect` cancel both and wait for them to let
  go of the USB session before going on, so nothing of an old connection keeps
  using its session beside the next one. An upload canceled this way leaves its
  partial session on the mixer until the next update starts.

Global processing and path selection are single writes. Each
names the revision and engine generation it was based on and requires a 202
with a valid state. A 409 or an uncertain outcome is reported to the caller and
not retried. Request bodies have size limits, and replies are parsed with the
`core` package's strict decoder, with its duplicate-key and depth checks.

`Transport.Close` cancels its work without sending queued writes. A write the
mixer has already received stays applied.

Built with the `mockmixer` tag, a simulated mixer takes USB 1's place (see
[Without a mixer](../../docs/developer/commands.md#without-a-mixer)).

## Running

From `desktop/`:

```sh
go test ./transport/...
go test -tags mockmixer ./transport/...
go run ./cmd/usb-probe   # needs a connected mixer
```
