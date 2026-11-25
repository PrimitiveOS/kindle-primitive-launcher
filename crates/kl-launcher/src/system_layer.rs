//! The launcher as the system layer over an app (`docs/system-architecture.md` in the umbrella repo).
//!
//! When the supervisor offers it (`KL_SYSTEM_LAYER=1`), opening an app does not end the launcher: the launcher asks
//! the supervisor to run the app (`SIGUSR1`) and keeps running with its drawing paused. It takes the screen back for
//! the system's own screens (quick settings, the sleep picture): the app is frozen, the screen is saved, and both are
//! put back when the system screen closes, so the app does not need to redraw.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

use kl_core::{Input, Rotation};
use kl_kindle::{log, Ev};

/// How long the supervisor gets to start the app before the request is sent again, and before the launcher gives up.
const RESEND_AFTER: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(15);

/// An app the supervisor runs while the launcher stays in charge.
pub struct AppSession {
    /// What was on the screen when the system took it (panel orientation), while a system screen is up. `None`
    /// while the app is in front.
    pub saved: Option<Vec<u8>>,
    /// The theme when the system screen opened: a change asks the app to redraw when it comes back.
    pub dark_mode_at_open: bool,
    /// The rotation when the system screen opened: after a turn (in Settings) the saved screen is the wrong way up.
    pub rotation_at_open: Rotation,
}

impl AppSession {
    pub fn new() -> AppSession {
        AppSession { saved: None, dark_mode_at_open: false, rotation_at_open: Rotation::Upright }
    }

    /// The app is in front: no system screen is up.
    pub fn in_front(&self) -> bool {
        self.saved.is_none()
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
        log(&format!("system layer: could not signal the supervisor (pid {pid})"));
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

/// Close the open app the way the Kindle UI does: TERM to its process group. The app session then ends.
pub fn end_app(run: &Path) {
    signal_app(run, libc::SIGTERM);
}

/// Freeze the app (`true`) while a system screen is up, or let it run again (`false`).
pub fn freeze(run: &Path, on: bool) {
    signal_app(run, if on { libc::SIGSTOP } else { libc::SIGCONT });
}

/// Ask the app to draw its whole screen again (the theme changed while it was frozen). Only an app that declares
/// `# Wake: redraw` gets the signal (the supervisor marks it with `run/app-redraws`): SIGUSR1 ends any other process.
/// Returns whether the app was asked.
pub fn ask_redraw(run: &Path) -> bool {
    if !can_redraw(run) {
        return false;
    }
    signal_app(run, libc::SIGUSR1);
    true
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
fn signal_app(run: &Path, signal: libc::c_int) {
    let Some(pid) = read_pid(&run.join("app.pid")) else { return };
    // SAFETY: kill takes no pointers. A negative pid addresses the process group the app leads.
    if unsafe { libc::kill(-pid, signal) } != 0 {
        // SAFETY: as above, for the app's own process.
        unsafe { libc::kill(pid, signal) };
    }
}

/// An app the supervisor runs is still alive (its `app.pid` names a live process): another must not start yet.
pub fn app_running(run: &Path) -> bool {
    // SAFETY: kill with signal 0 only checks that the process exists.
    read_pid(&run.join("app.pid")).is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok().filter(|pid: &i32| *pid > 1)
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

    #[test]
    fn upright_keeps_an_upright_panel_and_turns_a_half_turned_one() {
        assert_eq!(upright(&[1, 2, 3, 4], Rotation::Upright, 0), vec![1, 2, 3, 4]);
        assert_eq!(upright(&[1, 2, 3, 4], Rotation::UpsideDown, 0), vec![4, 3, 2, 1]);
        assert_eq!(upright(&[1, 2, 3, 4], Rotation::UpsideDown, 2), vec![1, 2, 3, 4], "two half turns cancel");
    }

    #[test]
    fn the_other_theme_is_the_picture_inverted() {
        let mut picture = vec![0u8, 17, 255];
        in_other_theme(&mut picture);
        assert_eq!(picture, vec![255, 238, 0]);
    }

    #[test]
    fn only_a_swipe_down_from_the_top_opens_quick_settings() {
        assert!(opens_quick_settings(Input::Swipe { dx: 10.0, dy: 400.0, start_y: 20.0 }, 120));
        assert!(!opens_quick_settings(Input::Swipe { dx: 10.0, dy: 400.0, start_y: 600.0 }, 120), "mid-screen is the app's");
        assert!(!opens_quick_settings(Input::Swipe { dx: 400.0, dy: 10.0, start_y: 20.0 }, 120), "sideways is the app's");
        assert!(!opens_quick_settings(Input::Tap(10.0, 10.0), 120));
    }
}
