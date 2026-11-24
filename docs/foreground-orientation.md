# Foreground portrait orientation

Quick Settings uses the foreground app’s effective portrait turn for its top-edge gesture, backdrop and overlay controls. Launcher keeps its own screen orientation setting for its home grid and other screens outside the app. A saved backdrop is tied to the same live app process, its effective turn and the framebuffer turn at capture. Launcher waits for that process to stop before reading the panel and verifies the owner again before restoring the frame or asking the app to redraw.

## App kit handoff

Launcher 0.6.22 and the existing native app kit both write a bare `U` or `D` to `orientation`. The Browser also reads that filename as a Launcher rotation fallback. Its format and consumers stay compatible. A bare turn cannot identify the foreground app that presented the pixels. [Launcher 0.6.22 foreground dispatch](https://github.com/PrimitiveOS/kindle-primitive-launcher/blob/155ce3fd6e520d9c4fc69e1a39d9c157d5135fc8/crates/kl-launcher/src/main.rs#L657-L678) · [Reader’s pinned app-kit writer](https://github.com/PrimitiveOS/kindle-primtitive-tools/blob/50035cf068c0157abf97477ef64eb28bc984b6b7/crates/primitive-app/src/lib.rs#L369-L373)

1. The supervisor gives each app run its own `KL_APP_TURN_FILE`. It atomically publishes the matching app PID, Linux process start time and run directory in `app-turn-owner`. The app gets a new directory on restart. The supervisor clears the record and directory when the app exits.
2. A compatible app kit atomically publishes a line such as `v1 D fixed` after the corresponding frame is presented. `U` and `D` describe logical upright and upside-down portrait. `fixed` keeps the app’s turn during Launcher rotation changes. `follow` uses the inherited Launcher turn. The kit must republish a policy change even when the displayed turn stays the same.
3. Launcher accepts the record only when its PID and process start time match the live foreground child. An absent or invalid record takes the existing Launcher fallback. Launcher restores a compatible saved frame, compensating for a changed firmware turn. A dead or replaced process cannot receive the old frame or its resume signal.

This patch provides the supervisor and Launcher consumer. Reader needs a new build with a compatible Tools publisher to enable the handoff. The pinned Launcher Tools revision is `487d245` and Reader’s orientation writer is `50035cf`. A new writer can use the scoped path when supplied and keep the existing path for older Launchers. Landscape and per-book orientation tracking are separate changes.

## Verification

The Launcher tests cover both portrait directions, opposite physical edges, rotated taps and page buttons, malformed and stale records, PID reuse, process death and frozen-session validity. The pixel checks project a changed overlay pixel into the original panel frame for both app turns and both supported firmware turns. They verify restoration across theme, Launcher turn and framebuffer changes. The supervisor fixture launches the same app twice, checks its session record and confirms the old record and directory are removed. Foreground gesture, sleep and wake acceptance on Kindle remains pending.
