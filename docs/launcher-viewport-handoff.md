# Launcher system surfaces over an app viewport

The experimental Launcher path presents Quick Settings, Settings, Battery,
and Power in the foreground app's logical turn. It uses the normal controls
and settings actions at the projected bounds. Launcher Home keeps its
portrait renderer, grid, and wallpaper caches across the overlay.

The supervisor offers v2 only during an explicit test run. It requires a
Launcher built with the `viewport-handoff` feature, the regular copied native
Reader entry, its storage lease and wake redraw declarations, and
`# PrimitiveViewportHandoff: 2`. Set `KL_EXPERIMENTAL_VIEWPORT_HANDOFF=1` for a
test run. The supervisor validates the offer against the foreground PID,
start tick, and unique run named by `app-turn-owner`. Runs without the offer
use the established portrait path.

Launcher validates the app's ready generation and current framebuffer,
maps the opening gesture, and stops that same app before snapshotting.
It revalidates the stopped owner, logical turn and dimensions, native
turn, row pitch, and y-offset before accepting input and on both sides of
projected framebuffer writes. Ordinary control repainting projects the
changed region. Dismissing the surface restores the saved native pixels
when the stopped frame and appearance are unchanged. Theme or rotation
changes require a fresh app draw. An interrupted write releases the app and
touch capture, requests a fresh app frame when storage is available, and
discards the old capture.

A per-run watermark quarantines the old ready generation before any
overlay or stock takeover can cover it. Exact restoration clears the
watermark. When a newer app generation is ready, Launcher can accept that
frame and discard the watermark. This also applies when Launcher restarts
over the same Reader process. The supervisor removes the watermark and its
temporary file at the end of the run.

Sleep and USB cancel the overlay without restoring its captured pixels.
A wake or eject requests an app redraw once both sleep and USB ownership
have ended. When a Launcher sleep wallpaper is configured, Launcher can
draw it once at the saved portrait rotation with a live stopped-owner
and framebuffer check. During drive mode the app's storage lifecycle
controls the drive artwork and reopens the reading position after eject.
Opening the Kindle Library projects the first waiting frame into the
saved Launcher portrait turn for continuity with the supervisor's
progress display. Foreground app exit returns drawing ownership to the
Launcher grid.

## Verification and activation

Use a temporary local Tools checkout at `4f28eb6` under `tools/` and run
`cargo test --locked --workspace --features viewport-handoff`. The
shipped build uses the committed Tools pin. Run
`sh tests/supervisor-test.sh` to check correct offer eligibility, the
default-off and legacy paths, and scoped cleanup.

The fake framebuffer cases render Quick Settings and exercise logical touch
and key mapping across all four turns and both supported firmware turns.
They check Settings, Battery, and Power controls, packed pixel restoration,
stale input rejection, a changed framebuffer during paint or restore,
theme changes, process restart, app replacement, overlapping sleep and
USB events, custom sleep wallpaper, and the portrait waiting frame.

Physical activation still requires a Reader entry using the same Tools
viewport protocol and Oasis validation of the following behavior.

- Open Settings and Battery from Quick Settings in both landscape
  directions. Check brightness, touch targets, Settings scrolling,
  appearance changes, Power cancellation, and return to the same page.
- Verify that physical page buttons work in the overlay and that any
  presses during the overlay do not page Reader on resume. The simulated
  mapping covers the Launcher queue. The Kindle's separate evdev queue
  and power-button path still require physical verification.
- Verify reader page and key behavior after stock sleep and custom
  wallpaper sleep, after USB drive mount/eject, and after overlapping
  sleep and USB. Verify that the expected artwork remains visible until
  that writer hands over the screen.
- Restart Launcher while an overlay is visible, then reopen Quick
  Settings over a fresh Reader frame. Verify that Reader exit restores
  the portrait grid and the Kindle Library exit shows a single waiting
  dialog through the transition.
- Run the same portrait paths with the experiment disabled. Restore the
  experiment to its default-off setting after testing.
