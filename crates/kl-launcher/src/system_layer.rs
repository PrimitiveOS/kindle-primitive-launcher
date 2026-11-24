//! The launcher as the system layer over an app (`docs/system-architecture.md` in the umbrella repo).
//!
//! When the supervisor offers it (`KL_SYSTEM_LAYER=1`), opening an app does not end the launcher: the launcher asks
//! the supervisor to run the app (`SIGUSR1`) and keeps running with its drawing paused. It takes the screen back for
//! the system's own screens (quick settings, the sleep picture): the app is frozen, the screen is saved, and both are
//! put back when the system screen closes, so the app does not need to redraw.

use std::{
    fs,
    io::Read,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

use kl_core::{Input, Rect, Rotation, Settings};
use kl_kindle::{display::Display, input, log, profile, Ev};

/// How long the supervisor gets to start the app before the request is sent again, and before the launcher gives up.
const RESEND_AFTER: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(15);

/// An app the supervisor runs while the launcher stays in charge.
pub struct AppSession {
    /// The saved frame and its owner while a system screen is up.
    pub saved: Option<AppScreen>,
}

impl AppSession {
    pub fn new() -> AppSession {
        AppSession { saved: None }
    }

    /// The app is in front: no system screen is up.
    pub fn in_front(&self) -> bool {
        self.saved.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppProcess {
    pid: i32,
    started: u64,
}

#[cfg(feature = "viewport-handoff")]
impl AppProcess {
    pub fn matches_viewport_owner(self, owner: &kl_kindle::viewport::ViewportOwner) -> bool {
        self.pid as u32 == owner.pid && self.started == owner.started_at
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppTurn {
    pub rotation: Rotation,
    follows_launcher: bool,
}

impl AppTurn {
    pub fn inherited(rotation: Rotation) -> Self {
        Self {
            rotation,
            follows_launcher: true,
        }
    }
}

pub struct AppScreen {
    pub process: AppProcess,
    pub turn: AppTurn,
    framebuffer_turn: u32,
    launcher_turn: Rotation,
    dark_mode: bool,
    pixels: Option<Vec<u8>>,
}

impl AppScreen {
    pub fn capture(
        process: AppProcess,
        turn: AppTurn,
        framebuffer_turn: u32,
        settings: &Settings,
        pixels: Option<Vec<u8>>,
    ) -> Self {
        Self {
            process,
            turn,
            framebuffer_turn,
            launcher_turn: settings.rotation,
            dark_mode: settings.dark_mode,
            pixels: pixels.filter(|frame| !frame.is_empty() && matches!(framebuffer_turn, 0 | 2)),
        }
    }

    pub fn backdrop(&self, dark_mode: bool, redraws: bool) -> Option<Vec<u8>> {
        let mut picture = upright(
            self.pixels.as_deref()?,
            self.turn.rotation,
            self.framebuffer_turn,
        );
        if dark_mode != self.dark_mode && redraws {
            in_other_theme(&mut picture);
        }
        Some(picture)
    }

    pub fn resume(
        self,
        settings: &Settings,
        framebuffer_turn: u32,
        redraws: bool,
    ) -> (Option<Vec<u8>>, bool) {
        let rotation =
            if redraws && self.turn.follows_launcher && self.launcher_turn != settings.rotation {
                settings.rotation
            } else {
                self.turn.rotation
            };
        let theme_changed = settings.dark_mode != self.dark_mode;
        let turn_changed = rotation != self.turn.rotation;
        let firmware_changed = framebuffer_turn != self.framebuffer_turn;
        let picture = self
            .pixels
            .filter(|_| matches!(framebuffer_turn, 0 | 2))
            .map(|mut picture| {
                if theme_changed && redraws {
                    in_other_theme(&mut picture);
                }
                if turn_changed != firmware_changed {
                    picture.reverse();
                }
                picture
            });
        let needs_redraw = picture.is_none()
            || theme_changed
            || self.launcher_turn != settings.rotation
            || firmware_changed;
        (picture, needs_redraw)
    }
}

/// The overlay takes the app's turn while Launcher keeps its own Settings value.
pub struct OverlayDisplay {
    panel: Display,
    pub turn: Option<Rotation>,
}

impl OverlayDisplay {
    pub fn open() -> Self {
        Self {
            panel: Display::open(),
            turn: None,
        }
    }

    pub fn show(&mut self, pixels: Vec<u8>, settings: &Settings, full: bool) {
        let mut output = settings.clone();
        output.rotation = self.turn.unwrap_or(settings.rotation);
        self.panel.show(pixels, &output, full);
    }

    pub fn show_region(&mut self, pixels: Vec<u8>, rect: Rect, settings: &Settings) {
        let mut output = settings.clone();
        output.rotation = self.turn.unwrap_or(settings.rotation);
        self.panel.show_region(pixels, rect, &output);
    }
}

impl Deref for OverlayDisplay {
    type Target = Display;

    fn deref(&self) -> &Display {
        &self.panel
    }
}

impl DerefMut for OverlayDisplay {
    fn deref_mut(&mut self) -> &mut Display {
        &mut self.panel
    }
}

fn read_record(path: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(129)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= 128).then_some(text)
}

fn process_state(run: &Path, proc: &Path) -> Option<(AppProcess, bool)> {
    let pid = read_pid(&run.join("app.pid"))?;
    let stat = fs::read_to_string(proc.join(pid.to_string()).join("stat")).ok()?;
    let (name, fields) = stat.rsplit_once(") ")?;
    if !name.starts_with(&format!("{pid} (")) {
        return None;
    }
    let mut fields = fields.split_ascii_whitespace();
    let state = fields.next()?;
    if !matches!(state, "R" | "S" | "D" | "T" | "t" | "W" | "K" | "P" | "I") {
        return None;
    }
    let started = fields.nth(18)?.parse().ok()?;
    Some((AppProcess { pid, started }, matches!(state, "T" | "t")))
}

pub fn foreground_process(run: &Path) -> Option<AppProcess> {
    process_state(run, Path::new("/proc")).map(|(process, _)| process)
}

fn session_turn(run: &Path, proc: &Path, process: AppProcess) -> Option<AppTurn> {
    let owner = read_record(&run.join("app-turn-owner"))?;
    let mut owner = owner.split_ascii_whitespace();
    if owner.next()? != "v1"
        || owner.next()?.parse::<i32>().ok()? != process.pid
        || owner.next()?.parse::<u64>().ok()? != process.started
    {
        return None;
    }
    let dir = owner.next()?;
    let token = dir.strip_prefix("app-turn.")?;
    if token.len() != 6
        || !token.bytes().all(|c| c.is_ascii_alphanumeric())
        || owner.next().is_some()
    {
        return None;
    }
    let turn = read_record(&run.join(dir).join("turn"))?;
    let mut turn = turn.split_ascii_whitespace();
    if turn.next()? != "v1" {
        return None;
    }
    let rotation = match turn.next()? {
        "U" => Rotation::Upright,
        "D" => Rotation::UpsideDown,
        _ => return None,
    };
    let follows_launcher = match turn.next()? {
        "follow" => true,
        "fixed" => false,
        _ => return None,
    };
    if turn.next().is_some() || process_state(run, proc)?.0 != process {
        return None;
    }
    Some(AppTurn {
        rotation,
        follows_launcher,
    })
}

pub fn foreground_turn(run: &Path, process: AppProcess, inherited: Rotation) -> AppTurn {
    session_turn(run, Path::new("/proc"), process).unwrap_or(AppTurn::inherited(inherited))
}

/// The Launcher input thread reports native coordinates. Map them to the active screen once.
pub fn screen_input(input: Input, turn: Rotation) -> Input {
    if turn == Rotation::Upright {
        return input;
    }
    match input {
        Input::Tap(x, y) => {
            let (x, y) = input::turn_point(turn.turn(), x, y);
            Input::Tap(x, y)
        }
        Input::LongTap(x, y) => {
            let (x, y) = input::turn_point(turn.turn(), x, y);
            Input::LongTap(x, y)
        }
        Input::Swipe { dx, dy, start_y } => Input::Swipe {
            dx: -dx,
            dy: -dy,
            start_y: (profile::ACTIVE.screen_h - 1) as f32 - start_y,
        },
        Input::PageForward => Input::PageBack,
        Input::PageBack => Input::PageForward,
        _ => input,
    }
}

/// The supervisor offers the system layer.
pub fn enabled() -> bool {
    std::env::var("KL_SYSTEM_LAYER").as_deref() == Ok("1")
}

/// The supervisor started the launcher again while an app runs (the launcher before it ended): the launcher follows
/// that app at once, with its drawing paused, instead of drawing the home screen over it.
pub fn attaching(run: &Path) -> bool {
    enabled() && std::env::var("KL_ATTACH_APP").as_deref() == Ok("1") && app_running(run)
}

/// Ask the supervisor to run the app described in `run/next`. False when the supervisor cannot be reached.
pub fn request_launch(run: &Path) -> bool {
    let Some(pid) = read_pid(&run.join("supervisor.pid")) else {
        log("system layer: no supervisor.pid");
        return false;
    };
    // SAFETY: kill takes no pointers; SIGUSR1 is the supervisor's "run the app" request.
    let sent = unsafe { libc::kill(pid, libc::SIGUSR1) } == 0;
    if !sent {
        log(&format!(
            "system layer: could not signal the supervisor (pid {pid})"
        ));
    }
    sent
}

/// Follow the app the supervisor runs: wait for `run/app.pid` to appear (asking again once if it is slow), then for it
/// to go, and send `Ev::AppEnded`.
pub fn watch(run: PathBuf, tx: Sender<Ev>) {
    thread::spawn(move || {
        let pid_file = run.join("app.pid");
        let started = Instant::now();
        let mut asked_again = false;
        while !pid_file.exists() {
            if !asked_again && started.elapsed() > RESEND_AFTER && run.join("next").exists() {
                log("system layer: the app has not started; asking the supervisor again");
                request_launch(&run);
                asked_again = true;
            }
            if started.elapsed() > GIVE_UP_AFTER {
                log("system layer: the supervisor did not start the app");
                let _ = tx.send(Ev::AppEnded);
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        while pid_file.exists() {
            thread::sleep(Duration::from_millis(300));
        }
        let _ = tx.send(Ev::AppEnded);
    });
}

/// Freeze the app (`true`) while a system screen is up, or let it run again (`false`).
pub fn freeze(run: &Path, process: AppProcess, on: bool) -> bool {
    let sent = signal_app(run, process, if on { libc::SIGSTOP } else { libc::SIGCONT });
    if !sent || !on {
        return sent;
    }
    // Wait briefly for the process to stop before reading the shared framebuffer.
    let deadline = Instant::now() + Duration::from_millis(24);
    loop {
        match process_state(run, Path::new("/proc")) {
            Some((active, true)) if active == process => return true,
            Some((active, false)) if active == process && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            _ => {
                signal_app(run, process, libc::SIGCONT);
                return false;
            }
        }
    }
}

/// Ask the app to draw its whole screen again (the theme changed while it was frozen). Only an app that declares
/// `# Wake: redraw` gets the signal (the supervisor marks it with `run/app-redraws`): SIGUSR1 ends any other process.
/// Returns whether the app was asked.
pub fn ask_redraw(run: &Path, process: AppProcess) -> bool {
    can_redraw(run) && signal_app(run, process, libc::SIGUSR1)
}

/// The app declares `# Wake: redraw`: it draws its whole screen again on SIGUSR1, in the current theme.
pub fn can_redraw(run: &Path) -> bool {
    run.join("app-redraws").exists()
}

/// The app's screen as it will look in the other theme: dark mode is the whole screen inverted.
pub fn in_other_theme(picture: &mut [u8]) {
    for px in picture.iter_mut() {
        *px = 255 - *px;
    }
}

/// Signal the app's process group (the supervisor starts apps in their own group), or the app alone without one.
fn signal_app(run: &Path, process: AppProcess, signal: libc::c_int) -> bool {
    if foreground_process(run) != Some(process) {
        return false;
    }
    // SAFETY: kill takes no pointers. A negative pid addresses the process group the app leads.
    unsafe { libc::kill(-process.pid, signal) == 0 || libc::kill(process.pid, signal) == 0 }
}

/// An app the supervisor runs is still alive (its `app.pid` names a live process): another must not start yet.
pub fn app_running(run: &Path) -> bool {
    // SAFETY: kill with signal 0 only checks that the process exists.
    read_pid(&run.join("app.pid")).is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid: &i32| *pid > 1)
}

/// The swipe that opens quick settings: down from the top edge, as on the home screen (`Ui::home`).
pub fn opens_quick_settings(input: Input, status_bottom: i32) -> bool {
    matches!(input, Input::Swipe { dx, dy, start_y }
        if dy > 150.0 && dy.abs() > dx.abs() * 1.5 && start_y <= status_bottom as f32 + 40.0)
}

/// The app's screen upright, from the panel's memory (`saved`): the panel is upright or a half turn, and a half turn
/// is its own inverse. The renderer draws the system's pop-ups over it (`Renderer::set_backdrop`).
pub fn upright(saved: &[u8], rotation: Rotation, fb_turn: u32) -> Vec<u8> {
    if (rotation.turn() + fb_turn) % 4 == 2 {
        saved.iter().rev().copied().collect()
    } else {
        saved.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SessionFiles(PathBuf);

    impl SessionFiles {
        fn new() -> Self {
            static ID: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "launcher-app-turn-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir_all(root.join("proc/31")).unwrap();
            fs::create_dir_all(root.join("app-turn.ABC123")).unwrap();
            Self(root)
        }

        fn put(&self, path: &str, text: impl AsRef<[u8]>) {
            fs::write(self.0.join(path), text).unwrap();
        }

        fn stat(&self, state: char, started: u64) {
            let middle = "0 ".repeat(18);
            self.put(
                "proc/31/stat",
                format!("31 (app ) script) {state} {middle}{started} 0\n"),
            );
        }

        fn process(&self) -> Option<(AppProcess, bool)> {
            process_state(&self.0, &self.0.join("proc"))
        }

        fn turn(&self, process: AppProcess) -> Option<AppTurn> {
            session_turn(&self.0, &self.0.join("proc"), process)
        }
    }

    impl Drop for SessionFiles {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_turn_belongs_to_one_live_process_and_survives_its_freeze() {
        let files = SessionFiles::new();
        let process = AppProcess {
            pid: 31,
            started: 75,
        };
        files.put("app.pid", "31\n");
        files.stat('S', 75);
        assert_eq!(files.process(), Some((process, false)));
        files.put("orientation", "D");
        assert_eq!(
            files.turn(process),
            None,
            "legacy orientation has no foreground owner"
        );
        files.put("app-turn-owner", "v1 31 75 app-turn.ABC123\n");
        files.put("app-turn.ABC123/turn", "v1 D fixed\n");
        let down = AppTurn {
            rotation: Rotation::UpsideDown,
            follows_launcher: false,
        };
        assert_eq!(files.turn(process), Some(down));
        files.stat('T', 75);
        assert_eq!(files.process(), Some((process, true)));
        assert_eq!(files.turn(process), Some(down));
        files.put("app-turn.ABC123/turn", "v1 U follow\n");
        assert_eq!(
            files.turn(process),
            Some(AppTurn::inherited(Rotation::Upright))
        );

        files.stat('Z', 75);
        assert_eq!(files.process(), None);
        assert_eq!(files.turn(process), None);
        files.stat('S', 99);
        let replacement = files.process().unwrap().0;
        assert_eq!(
            files.turn(process),
            None,
            "PID reuse invalidates an old snapshot owner"
        );
        assert_eq!(
            files.turn(replacement),
            None,
            "the old marker does not identify the replacement"
        );
        files.put("app-turn-owner", "v1 31 99 app-turn.DEF456\n");
        assert_eq!(
            files.turn(replacement),
            None,
            "a new session cannot read the previous app's file"
        );
        fs::remove_file(files.0.join("app.pid")).unwrap();
        assert_eq!(files.process(), None);
    }

    #[test]
    fn malformed_owner_and_turn_records_fall_back() {
        let files = SessionFiles::new();
        files.put("app.pid", "31");
        files.stat('S', 75);
        let process = files.process().unwrap().0;
        files.put("app-turn.ABC123/turn", "v1 U follow");
        for invalid in [
            "v2 31 75 app-turn.ABC123",
            "v1 32 75 app-turn.ABC123",
            "v1 31 0 app-turn.ABC123",
            "v1 31 75 app-turn./a/b/c",
            "v1 31 75 ../app-turn.ABC123",
            "v1 31 75 app-turn.ABC123 more",
        ] {
            files.put("app-turn-owner", invalid);
            assert_eq!(files.turn(process), None, "{invalid}");
        }
        files.put("app-turn-owner", "v1 31 75 app-turn.ABC123");
        for invalid in [
            "",
            "U",
            "v1 L follow",
            "v1 U auto",
            "v1 D fixed extra",
            "v2 D fixed",
        ] {
            files.put("app-turn.ABC123/turn", invalid);
            assert_eq!(files.turn(process), None, "{invalid}");
        }
        files.put("app-turn.ABC123/turn", "\n".repeat(129));
        assert_eq!(files.turn(process), None);
        files.put("app-turn.ABC123/turn", [0xff]);
        assert_eq!(files.turn(process), None);
        files.put("app.pid", "-31");
        assert_eq!(files.process(), None);
    }

    #[test]
    fn gestures_and_keys_use_the_turn_of_the_visible_screen() {
        let swipe = |start_y, dy| Input::Swipe {
            dx: 0.,
            dy,
            start_y,
        };
        let height = profile::ACTIVE.screen_h as f32;
        let up = swipe(19., 260.);
        let down = swipe(height - 20., -260.);
        for (turn, good, opposite) in [
            (Rotation::Upright, up, down),
            (Rotation::UpsideDown, down, up),
        ] {
            assert_eq!(screen_input(good, turn), up);
            assert!(opens_quick_settings(screen_input(good, turn), 120));
            assert!(!opens_quick_settings(screen_input(opposite, turn), 120));
            assert_eq!(screen_input(Input::Sleep, turn), Input::Sleep);
            assert_eq!(screen_input(Input::Wake, turn), Input::Wake);
        }
        let opposite = screen_input(Input::Tap(0., 0.), Rotation::UpsideDown);
        assert_eq!(
            opposite,
            Input::Tap((profile::ACTIVE.screen_w - 1) as f32, height - 1.)
        );
        assert_eq!(
            screen_input(opposite, Rotation::UpsideDown),
            Input::Tap(0., 0.)
        );
        assert_eq!(
            screen_input(Input::PageForward, Rotation::UpsideDown),
            Input::PageBack
        );
        assert_eq!(
            screen_input(Input::PageBack, Rotation::UpsideDown),
            Input::PageForward
        );
        assert_eq!(
            screen_input(Input::PageBack, Rotation::Upright),
            Input::PageBack
        );
    }

    #[test]
    fn overlay_and_resume_preserve_pixels_in_both_panel_turns() {
        let settings = Settings::default();
        let process = AppProcess {
            pid: 31,
            started: 75,
        };
        let logical = vec![10, 20, 30, 40, 50, 60];
        for rotation in [Rotation::Upright, Rotation::UpsideDown] {
            for firmware in [0, 2] {
                let app = AppTurn {
                    rotation,
                    follows_launcher: false,
                };
                let raw = kl_render::for_panel(logical.clone(), rotation, firmware);
                let saved =
                    || AppScreen::capture(process, app, firmware, &settings, Some(raw.clone()));
                assert_eq!(
                    saved().backdrop(settings.dark_mode, true),
                    Some(logical.clone())
                );
                let mut overlay = saved().backdrop(settings.dark_mode, true).unwrap();
                overlay[1] = 90;
                let shown = kl_render::for_panel(overlay, rotation, firmware);
                let expected = if (rotation.turn() + firmware) % 4 == 2 {
                    4
                } else {
                    1
                };
                assert_eq!(shown[expected], 90);
                assert_eq!(shown.iter().zip(&raw).filter(|(a, b)| a != b).count(), 1);
                assert_eq!(
                    saved().resume(&settings, firmware, true),
                    (Some(raw), false)
                );
            }
        }
    }

    #[test]
    fn resume_keeps_fixed_turns_and_applies_inherited_system_changes() {
        let settings = Settings::default();
        let mut changed = settings.clone();
        changed.rotation = Rotation::UpsideDown;
        changed.dark_mode = true;
        let process = AppProcess {
            pid: 31,
            started: 75,
        };
        let make = |follows_launcher| {
            AppScreen::capture(
                process,
                AppTurn {
                    rotation: Rotation::Upright,
                    follows_launcher,
                },
                0,
                &settings,
                Some(vec![10, 20, 30, 40]),
            )
        };
        assert_eq!(
            make(false).resume(&changed, 0, true),
            (Some(vec![245, 235, 225, 215]), true)
        );
        assert_eq!(
            make(true).resume(&changed, 0, true),
            (Some(vec![215, 225, 235, 245]), true)
        );
        assert_eq!(
            make(true).resume(&changed, 2, true),
            (Some(vec![245, 235, 225, 215]), true)
        );
        assert_eq!(
            make(true).resume(&changed, 0, false),
            (Some(vec![10, 20, 30, 40]), true)
        );
        assert_eq!(make(true).resume(&changed, 1, true), (None, true));
        assert_eq!(
            AppScreen::capture(
                process,
                AppTurn::inherited(Rotation::Upright),
                0,
                &settings,
                None
            )
            .resume(&settings, 0, true),
            (None, true)
        );
    }

    #[test]
    fn upright_keeps_an_upright_panel_and_turns_a_half_turned_one() {
        assert_eq!(
            upright(&[1, 2, 3, 4], Rotation::Upright, 0),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            upright(&[1, 2, 3, 4], Rotation::UpsideDown, 0),
            vec![4, 3, 2, 1]
        );
        assert_eq!(
            upright(&[1, 2, 3, 4], Rotation::UpsideDown, 2),
            vec![1, 2, 3, 4],
            "two half turns cancel"
        );
    }

    #[test]
    fn the_other_theme_is_the_picture_inverted() {
        let mut picture = vec![0u8, 17, 255];
        in_other_theme(&mut picture);
        assert_eq!(picture, vec![255, 238, 0]);
    }

    #[test]
    fn only_a_swipe_down_from_the_top_opens_quick_settings() {
        assert!(opens_quick_settings(
            Input::Swipe {
                dx: 10.0,
                dy: 400.0,
                start_y: 20.0
            },
            120
        ));
        assert!(
            !opens_quick_settings(
                Input::Swipe {
                    dx: 10.0,
                    dy: 400.0,
                    start_y: 600.0
                },
                120
            ),
            "mid-screen is the app's"
        );
        assert!(
            !opens_quick_settings(
                Input::Swipe {
                    dx: 400.0,
                    dy: 10.0,
                    start_y: 20.0
                },
                120
            ),
            "sideways is the app's"
        );
        assert!(!opens_quick_settings(Input::Tap(10.0, 10.0), 120));
    }
}
