use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kl_core::{
    battery::{Reading, Sample, Source, HISTORY_SECS, LOW_POWER_LIGHT},
    storage_lease::{user_storage_is_mounted, StorageLease},
    Status,
};
use kl_kindle::{device, log};

const FILE_NAME: &str = "battery-history.txt";
const LIGHT_BEFORE_MODE: &str = "low-power-light";
const MAX_SAMPLES: usize = 300;
const MAX_FILE_BYTES: u64 = 32 * 1024;
const OBSERVE_EVERY: u64 = 5 * 60;
const MIN_GAP_SECS: u64 = 60;
const SAVE_EVERY: Duration = Duration::from_secs(120);

#[derive(Default)]
struct History {
    samples: Vec<Sample>,
    gap_pending: bool,
    dirty: bool,
}

impl History {
    fn prune(&mut self, now: u64) {
        let past = now.saturating_sub(HISTORY_SECS);
        self.samples.retain(|sample| (past..=now).contains(&sample.observed_at));
        let extra = self.samples.len().saturating_sub(MAX_SAMPLES);
        self.samples.drain(..extra);
    }

    fn observe(&mut self, now: u64, reading: Reading) -> bool {
        let previous_count = self.samples.len();
        if self.samples.last().is_some_and(|last| last.observed_at > now) {
            // After a clock correction, start a new segment at the verified current time.
            self.gap_pending = true;
        }
        self.prune(now);
        let removed = self.samples.len() != previous_count;
        self.dirty |= removed;
        let Some(percent) = reading.percent.filter(|value| *value <= 100) else {
            self.gap_pending = true;
            return removed;
        };
        let Some(last) = self.samples.last() else {
            self.record(now, percent, reading.source);
            return true;
        };
        let elapsed = now - last.observed_at;
        if elapsed == 0 {
            return removed;
        }
        if reading.source != last.source || elapsed >= OBSERVE_EVERY
            || (self.gap_pending && elapsed >= MIN_GAP_SECS)
        {
            self.gap_pending |= elapsed > OBSERVE_EVERY * 2;
            self.record(now, percent, reading.source);
            return true;
        }
        removed
    }

    fn record(&mut self, now: u64, percent: u32, source: Source) {
        self.samples.push(Sample {
            observed_at: now,
            percent,
            source,
            gap_before: std::mem::take(&mut self.gap_pending),
        });
        self.prune(now);
        self.dirty = true;
    }

    fn read(path: &Path, now: u64) -> Self {
        let mut history = Self { gap_pending: true, ..Self::default() };
        let Ok(file) = File::open(path) else { return history };
        let mut text = String::new();
        if file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).is_err()
            || text.len() as u64 > MAX_FILE_BYTES
        {
            return history;
        }
        history.samples = text.lines().filter_map(parse_sample).collect();
        history.samples.sort_by_key(|sample| sample.observed_at);
        history.samples.dedup_by_key(|sample| sample.observed_at);
        history.prune(now);
        history
    }

    fn encode(&self) -> String {
        let mut text = String::new();
        for sample in &self.samples {
            // The source names are part of the history file format.
            let source = match sample.source {
                Source::Unknown => "unknown",
                Source::OnBattery => "battery",
                Source::PluggedIn => "plugged",
                Source::Charging => "charging",
                Source::Full => "full",
            };
            let _ = writeln!(text, "{}\t{}\t{}\t{}", sample.observed_at, sample.percent, source, u8::from(sample.gap_before));
        }
        text
    }
}

fn parse_sample(line: &str) -> Option<Sample> {
    let mut fields = line.split('\t');
    let observed_at = fields.next()?.parse().ok()?;
    let percent = fields.next()?.parse::<u32>().ok().filter(|value| *value <= 100)?;
    let source = match fields.next()? {
        "unknown" => Source::Unknown,
        "battery" => Source::OnBattery,
        "plugged" => Source::PluggedIn,
        "charging" => Source::Charging,
        "full" => Source::Full,
        _ => return None,
    };
    let gap_before = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    fields.next().is_none().then_some(Sample { observed_at, percent, source, gap_before })
}

pub struct Runtime {
    run: PathBuf,
    history: History,
    lease: Option<StorageLease>,
    next_save: Instant,
}

impl Runtime {
    pub fn new(run: &Path, dir: &Path, now: u64, user_storage: bool) -> Self {
        let lease = (std::env::var("KL_LAUNCHER_USB_LEASE").as_deref() == Ok("1"))
            .then(|| StorageLease::new(&run.join("launcher-usb")));
        let history = if user_storage && user_storage_is_mounted() {
            History::read(&dir.join(FILE_NAME), now)
        } else {
            History { gap_pending: true, ..History::default() }
        };
        let wait = history.samples.last().map_or(Duration::ZERO, |sample| {
            SAVE_EVERY.saturating_sub(Duration::from_secs(now - sample.observed_at))
        });
        Self { run: run.into(), history, lease, next_save: Instant::now() + wait }
    }

    pub fn break_history(&mut self) {
        self.history.gap_pending = true;
    }

    pub fn merge_after_eject(&mut self, dir: &Path, now: u64) {
        if !user_storage_is_mounted() {
            return;
        }
        let mut saved = History::read(&dir.join(FILE_NAME), now);
        let first_observed = self.history.samples.first().map(|sample| sample.observed_at);
        if let Some(first_observed) = first_observed {
            saved.samples.retain(|sample| sample.observed_at < first_observed);
        }
        saved.samples.append(&mut self.history.samples);
        saved.prune(now);
        saved.dirty = self.history.dirty;
        self.history = saved;
    }

    pub fn observe(&mut self, now: u64, reading: Reading) -> bool {
        self.history.observe(now, reading)
    }

    pub fn populate(&self, status: &mut Status, now: u64) {
        status.battery_history.clone_from(&self.history.samples);
        status.battery_now = now;
    }

    pub fn save_if_due(&mut self, dir: &Path, user_storage: bool) {
        if !user_storage || !self.history.dirty || self.lease.is_none() || Instant::now() < self.next_save {
            return;
        }
        self.next_save = Instant::now() + SAVE_EVERY;
        match self.write_leased(&dir.join(FILE_NAME), &self.history.encode()) {
            Ok(()) => self.history.dirty = false,
            Err(error) => log(&format!("battery history: {error}")),
        }
    }

    pub(super) fn write_leased(&mut self, path: &Path, content: &str) -> io::Result<()> {
        self.with_storage(|| super::write_atomic(path, content))
    }

    pub fn with_storage<T>(&mut self, work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let lease = self.lease.as_mut().ok_or_else(|| io::Error::other("Open Launcher from the Kindle Library before saving this setting."))?;
        lease.begin_write()?;
        let write = work();
        // A completed rename is the commit point. If idle notification fails, the supervisor's
        // Launcher-exit cleanup will release the volume and the saved preference still applies.
        if let Err(error) = lease.release() {
            log(&format!("launcher storage release: {error}"));
        }
        write
    }

    pub fn take_storage_lease(&mut self) -> Option<StorageLease> {
        self.lease.take()
    }

    pub fn return_storage_lease(&mut self, lease: StorageLease) {
        self.lease = Some(lease);
    }

    /// The prior level lives only as long as this supervisor run. Compare the current setting
    /// before restoring so manual changes made in a separate app win.
    pub fn set_light_mode(&self, on: bool) -> io::Result<()> {
        let saved = self.run.join(LIGHT_BEFORE_MODE);
        if on {
            let current = device::light_level().ok_or_else(|| io::Error::other("Front light is unavailable."))?;
            if current > LOW_POWER_LIGHT {
                super::write_atomic(&saved, &current.to_string())?;
                if !device::try_set_light(LOW_POWER_LIGHT) {
                    let _ = fs::remove_file(saved);
                    return Err(io::Error::other("Could not lower the front light."));
                }
            }
        } else if let Ok(previous) = fs::read_to_string(&saved) {
            if let Ok(previous) = previous.trim().parse::<u32>() {
                if previous > LOW_POWER_LIGHT && previous <= device::LEVEL_MAX {
                    let current = device::light_level().ok_or_else(|| io::Error::other("Front light is unavailable."))?;
                    if current == LOW_POWER_LIGHT && !device::try_set_light(previous) {
                        return Err(io::Error::other("Could not restore the front light."));
                    }
                }
            }
            let _ = fs::remove_file(saved);
        }
        Ok(())
    }

    pub fn manually_changed_light(&self) {
        let _ = fs::remove_file(self.run.join(LIGHT_BEFORE_MODE));
    }

    pub fn observe_light(&self, status: &Status) {
        if status.low_power && status.display_support.front_light && status.light != LOW_POWER_LIGHT {
            self.manually_changed_light();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(percent: u32) -> Reading {
        Reading { percent: Some(percent), source: Source::OnBattery }
    }

    #[test]
    fn observations_are_spaced_and_annotate_real_source_transitions() {
        let mut history = History::default();
        assert!(history.observe(1_000, battery(72)));
        assert!(!history.observe(1_005, battery(71)));
        assert!(history.observe(1_010, Reading { percent: Some(71), source: Source::Charging }));
        assert_eq!(history.samples[1].source, Source::Charging);
        assert!(!history.samples[1].gap_before);
        assert!(history.observe(1_020 + OBSERVE_EVERY, battery(75)));
        assert_eq!(history.samples[2].percent, 75);
    }

    #[test]
    fn sleep_unknown_readings_and_clock_changes_break_the_series() {
        let mut history = History::default();
        history.observe(1_000, battery(90));
        history.gap_pending = true;
        assert!(history.observe(1_000 + MIN_GAP_SECS, battery(89)));
        assert!(history.samples.last().unwrap().gap_before);
        assert!(!history.observe(1_065, battery(101)));
        assert!(history.observe(1_800, battery(88)));
        assert!(history.samples.last().unwrap().gap_before);
        assert!(history.observe(500, battery(88)));
        assert_eq!(history.samples.len(), 1);
        assert!(history.samples[0].gap_before);
    }

    #[test]
    fn retains_only_the_current_day_and_loads_bounded_values() {
        let mut history = History::default();
        history.observe(1_000, battery(100));
        history.observe(1_000 + HISTORY_SECS + 1, battery(87));
        assert_eq!(history.samples.len(), 1);
        assert_eq!(history.encode().lines().filter_map(parse_sample).collect::<Vec<_>>(), history.samples);
        assert!(parse_sample("1000\t101\tbattery\t0").is_none());
        assert!(parse_sample("1000\t30\tinvalid\t0").is_none());
        assert!(parse_sample("1000\t30\tbattery\t2").is_none());
    }
}
