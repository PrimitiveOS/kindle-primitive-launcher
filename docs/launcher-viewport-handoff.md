# Quick Settings over a foreground viewport

The experimental Launcher path uses the foreground app's logical turn and
dimensions for Quick Settings. It retains a separate Quick Settings layout
and renderer, so the home grid and existing portrait overlay path can use
their usual geometry.

The supervisor offers v2 only during an explicit test run. It requires a
Launcher built with `viewport-handoff`, the regular copied native Reader
entry, its storage lease and wake redraw markers, and
`# PrimitiveViewportHandoff: 2`. The experimental flag defaults to off and
the current Reader entry does not advertise v2. The offer names the same
PID, start tick and scoped run as `app-turn-owner`.

For an offered run, Launcher checks a live ready record and the observed
framebuffer, maps the native gesture into its logical layout, and only then
tries to freeze the same app. It reads the exact ready record again after
the stop, captures the active native rows, and checks the stopped record
again before painting. Quick Settings uses the existing control hit regions
and renders through a geometry-bound display update. Dismissing the panel
restores its exact packed pixels only while the same stopped record and
geometry still match. App exit, pending records and ownership changes
invalidate the relevant queued input.

Sleep and USB drive mode release Quick Settings and quarantine its old
capture. When the stock writer has finished, an app redraw is requested.
Launcher waits for a newer ready generation from the same app or a new
authorized owner. An app without an offer continues through the existing
portrait path.

## Activation gates

The experiment remains off pending these changes and physical validation.

- Coordinate external display ownership across stock sleep, wake, USB and
  Launcher restart. Reader must publish a new ready generation after
  returning from a stock writer, even if the logical viewport is the same.
  Its temporary USB artwork and user storage transitions need the same
  ownership discipline.
- Connect Full Settings, Battery and Power to the foreground session
  lifecycle. The experimental Quick Settings panel keeps these navigation
  events inside the panel until those screens can return safely.
- Integrate Reader's logical viewport reflow and bound its first
  presentation to the negotiated run. Verify repeated rotations, physical
  keys and touch, app exit, sleep and USB on the Oasis.
- Land the Tools version with the v2 and geometry-bound display APIs
  before changing the Launcher dependency pin.

## Local verification

The normal build uses the committed Tools pin. For the feature tests, use
a temporary local Tools checkout at `67d1cd3` under `tools/`, then run
`cargo test --locked --workspace --features viewport-handoff`. Keep the
committed pin until the shared dependency is ready.

The tests run the actual Quick Settings layout and renderer for all four
logical turns under both supported firmware turns. They check top- and
bottom-edge gestures, brightness hit-testing, both page keys, and pixel-exact
restore. Additional cases simulate ready records changing around freeze and
capture, owner replacement, mismatched display geometry, and old records
after sleep or USB. `sh tests/supervisor-test.sh` checks the default-off
negotiation, the eligible run, and cleanup with simulated Kindle commands.
