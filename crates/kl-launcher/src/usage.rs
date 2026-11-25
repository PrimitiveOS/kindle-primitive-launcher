//! Records the battery use the Battery page shows: the time the screen was on and how often it woke (from the power
//! daemon's sleep and wake events), and each app's time in front and CPU time. Saved to `usage.txt` in the launcher
//! folder every few minutes, and when the screen sleeps. Nothing is sent anywhere.

use std::fs;
use std::path::{Path, PathBuf};

use kl_core::usage::{day_of, UsageLog};

/// How often a change is saved, at most.
const SAVE_EVERY_SECS: u64 = 300;

struct AppRun {
    name: String,
    started: u64,
    pid: Option<i32>,
    last_ticks: u64,
    ticks: u64,
}

pub struct Recorder {
    log: UsageLog,
    path: PathBuf,
    screen_on_since: Option<u64>,
    app: Option<AppRun>,
    dirty: bool,
    saved_at: u64,
}

impl Recorder {
    /// Read the saved log and keep only the last thirty days.
    pub fn load(dir: &Path, now: u64) -> Recorder {
        let path = dir.join("usage.txt");
        let mut log = UsageLog::parse(&fs::read_to_string(&path).unwrap_or_default());
        log.prune(day_of(now));
        Recorder { log, path, screen_on_since: None, app: None, dirty: false, saved_at: now }
    }

    /// The screen came on (a wake): its time starts now, and the wake counts.
    pub fn screen_on(&mut self, now: u64) {
        if self.screen_on_since.is_none() {
            self.screen_on_since = Some(now);
            self.log.add_wake(day_of(now));
            self.dirty = true;
        }
    }

    /// The screen went off: its time up to now is kept, and the log is saved.
    pub fn screen_off(&mut self, now: u64) {
        if let Some(since) = self.screen_on_since.take() {
            self.log.add_screen_on(day_of(now), now.saturating_sub(since));
            self.dirty = true;
            self.save(now);
        }
    }

    /// An app came to the front.
    pub fn app_started(&mut self, name: &str, now: u64) {
        self.app_ended(now);
        self.app = Some(AppRun { name: name.to_string(), started: now, pid: None, last_ticks: 0, ticks: 0 });
    }

    /// The app's CPU time so far (its process, from `/proc`). A new process starts counting from zero.
    pub fn sample_cpu(&mut self, pid: Option<i32>) {
        let Some(app) = self.app.as_mut() else { return };
        let Some(pid) = pid else { return };
        let Some(ticks) = cpu_ticks(pid) else { return };
        if app.pid != Some(pid) {
            app.pid = Some(pid);
            app.last_ticks = ticks;
            return;
        }
        if ticks >= app.last_ticks {
            app.ticks += ticks - app.last_ticks;
            app.last_ticks = ticks;
        }
    }

    /// The app left the front: its time and CPU are kept for its day.
    pub fn app_ended(&mut self, now: u64) {
        if let Some(app) = self.app.take() {
            self.log.add_app(day_of(now), &app.name, now.saturating_sub(app.started), app.ticks);
            self.dirty = true;
        }
    }

    /// Save if something changed and the last save is old enough.
    pub fn save_if_due(&mut self, now: u64) {
        if self.dirty && now.saturating_sub(self.saved_at) >= SAVE_EVERY_SECS {
            self.save(now);
        }
    }

    fn save(&mut self, now: u64) {
        if !self.dirty {
            return;
        }
        let tmp = self.path.with_extension("txt.tmp");
        if fs::write(&tmp, self.log.to_text()).and_then(|()| fs::rename(&tmp, &self.path)).is_ok() {
            self.dirty = false;
            self.saved_at = now;
        }
    }

    /// The log, as the Battery page reads it (the days kept, and the running totals).
    pub fn log(&self) -> &UsageLog {
        &self.log
    }
}

/// The Battery page's usage lines: the screen line, then the most used apps (the last seven days).
pub fn summary_lines(log: &UsageLog, now: u64) -> Vec<(String, String)> {
    let summary = log.summarize(day_of(now).saturating_sub(6));
    let mut lines = vec![("Screen on".to_string(), format!("{}, {} wakes", hours_minutes(summary.screen_on_secs), summary.wakes))];
    for app in summary.apps.iter().take(4) {
        lines.push((app.app.clone(), format!("about {}% · {}", app.share_percent, hours_minutes(app.front_secs))));
    }
    lines
}

fn hours_minutes(secs: u64) -> String {
    let minutes = secs / 60;
    if minutes >= 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    }
}

/// The app's name, from the entry it was asked to run (`documents/Snake.sh` is "Snake").
pub fn app_name(run: &Path) -> String {
    fs::read_to_string(run.join("app-request"))
        .ok()
        .and_then(|text| text.lines().next().map(str::to_string))
        .and_then(|entry| Path::new(entry.trim()).file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "App".to_string())
}

/// The CPU ticks (user and system) of a process, from its `/proc` stat line.
fn cpu_ticks(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    ticks_in_stat(&stat)
}

/// The utime and stime fields, which come after the command name's closing parenthesis.
fn ticks_in_stat(stat: &str) -> Option<u64> {
    let rest = stat.rsplit(')').next()?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the parenthesis the fields start at field 3 (the state): utime is field 14, stime field 15.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_ticks_come_from_the_utime_and_stime_fields() {
        // A stat line whose command name has spaces and a parenthesis: utime 300, stime 40.
        let stat = "77 (my (odd) app) S 1 77 77 0 -1 4194560 100 0 0 0 300 40 0 0 20 0 1 0";
        assert_eq!(ticks_in_stat(stat), Some(340));
        assert_eq!(ticks_in_stat("bad"), None);
    }

    #[test]
    fn the_app_name_is_the_entry_file_name() {
        let dir = std::env::temp_dir().join(format!("kl-usage-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app-request"), "/mnt/us/documents/Snake.sh\n").unwrap();
        assert_eq!(app_name(&dir), "Snake");
        fs::remove_file(dir.join("app-request")).unwrap();
        assert_eq!(app_name(&dir), "App");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_session_is_recorded_for_its_app_and_day() {
        let dir = std::env::temp_dir().join(format!("kl-usage-session-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let now = 86_400 * 50;
        let mut rec = Recorder::load(&dir, now);
        rec.screen_on(now);
        rec.app_started("Snake", now + 10);
        rec.app_ended(now + 70);
        rec.screen_off(now + 100);
        let log = rec.log();
        let day = &log.days[0];
        assert_eq!((day.screen_on_secs, day.wakes), (100, 1));
        assert_eq!(day.apps["Snake"].front_secs, 60);
        rec.save_if_due(now + 1_000);
        assert!(dir.join("usage.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
