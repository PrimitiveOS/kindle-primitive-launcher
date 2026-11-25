//! Primitive Launcher on the Kindle. Started by the supervisor (`launcher.sh`) with the stock framework
//! stopped. Draws the app grid on the e-ink panel and reads touch, page buttons and power events. To open
//! an app it writes `next` and exits with code 10; the supervisor runs the app and starts the launcher
//! again. Exit 0 asks the supervisor for the stock Kindle UI.
//!
//! Environment (defaults for the Kindle):
//!   KL_DIR        /mnt/us/launcher    apps.toml, settings.txt, fonts/, cache/, ssh.sh
//!   KL_RUN        /var/tmp/kl         next, heartbeat, orientation, fonts copied at start
//!   KL_DOCUMENTS  /mnt/us/documents   scriptlets

mod bootstrap;
mod clock_sync;
mod management;
mod power;
mod usage;
mod system_layer;
mod transfer;
mod wallpaper;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::Ordering,
        mpsc::{channel, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use kl_core::store_open;
use kl_core::layout::Page;
use kl_core::wallpaper::{WallpaperChoice, WallpaperTarget};
use kl_core::{discover, Action, App, Config, DeviceCmd, Input, Launch, Layout, Screen, Settings, Status, Ui};
use kl_render::Renderer;

use kl_kindle::{device, display::Display, input, log, profile, Ev};

const W: u32 = profile::ACTIVE.screen_w;
const H: u32 = profile::ACTIVE.screen_h;

/// Exit codes the supervisor acts on.
const EXIT_KINDLE_UI: i32 = 0;
const EXIT_LAUNCH: i32 = 10;


/// Join a network on a thread, then send the result and a fresh scan. The password is passed on, never logged.
fn start_wifi_join(tx: Sender<Ev>, name: String, secured: bool, password: Option<String>, saved: bool) {
    thread::spawn(move || {
        log(&format!("join {name} (saved: {saved}, secured: {secured})"));
        let result = device::join_network(&name, secured, password.as_deref(), saved);
        let (ok, message) = match result {
            Ok(()) => (true, format!("Connected to {name}")),
            Err(e) => (false, e),
        };
        log(&format!("join {name}: {}", if ok { "connected" } else { "failed" }));
        let _ = tx.send(Ev::WifiResult { ok, message });
        let _ = tx.send(Ev::Wifi(device::wifi_info()));
    });
}

/// Run an SSH job on a thread (with the launcher's folder), then send its message to the event loop as a notice.
fn start_ssh_job(tx: Sender<Ev>, job: impl FnOnce(&str) -> String + Send + 'static) {
    thread::spawn(move || {
        let message = job(&device::launcher_dir());
        let _ = tx.send(Ev::Notice(message));
    });
}

/// Forget a saved network on a thread, then send the result and a fresh scan.
fn start_wifi_forget(tx: Sender<Ev>, name: String) {
    thread::spawn(move || {
        let (ok, message) = match device::forget_network(&name) {
            Ok(()) => (true, format!("Forgot {name}")),
            Err(e) => (false, e),
        };
        log(&format!("forget {name}: {ok}"));
        let _ = tx.send(Ev::WifiResult { ok, message });
        let _ = tx.send(Ev::Wifi(device::wifi_info()));
    });
}

/// Scan for Wi-Fi on a thread and send the result to the event loop.
fn start_wifi_scan(tx: Sender<Ev>) {
    start_wifi_scan_after(tx, 0);
}

/// Scan after `secs` seconds (Wi-Fi just turned on needs a moment to come up).
fn start_wifi_scan_after(tx: Sender<Ev>, secs: u64) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(secs));
        let info = device::wifi_info();
        let _ = tx.send(Ev::Wifi(info));
    });
}


fn env_dir(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

/// Write a file through a temporary name, so a reader never sees half of it.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// The config, discovered apps and extensions, and any problems found reading them.
fn load_library(dir: &Path, documents: &Path) -> (Config, Vec<App>, Vec<kl_core::kual::MenuItem>, Vec<String>) {
    let (config, mut problems) = match fs::read_to_string(dir.join("apps.toml")) {
        Ok(text) => Config::parse(&text),
        Err(_) => (Config::default(), Vec::new()),
    };
    let (mut apps, more) = discover(documents, &config);
    problems.extend(more);
    kl_core::apps::use_koreader_wrapper(&mut apps, &config, &dir.join("run-koreader.sh"));
    let extensions_root = documents.parent().unwrap_or(Path::new("/mnt/us")).join("extensions");
    let (extensions, more) = kl_core::kual::discover_root(&extensions_root, kl_core::kual::OASIS_3_MODEL);
    problems.extend(more);
    (config, apps, extensions, problems)
}

fn font_paths(dir: &Path, run: &Path) -> Vec<String> {
    let names = ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"];
    let base = if run.join("fonts").join(names[0]).exists() { run.join("fonts") } else { dir.join("fonts") };
    names.iter().map(|n| base.join(n).to_string_lossy().into_owned()).collect()
}

extern "C" fn on_fatal(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    // Only write(2) here: safe in a signal handler. Copied from Primitive Browser.
    fn put(bytes: &[u8]) {
        unsafe { libc::write(2, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
    }
    let addr = if info.is_null() { 0 } else { unsafe { (*info).si_addr() as usize } };
    let mut buf = [0u8; 18];
    let mut at = buf.len();
    let mut v = addr;
    loop {
        at -= 1;
        buf[at] = b"0123456789abcdef"[v & 15];
        v >>= 4;
        if v == 0 {
            break;
        }
    }
    put(b"\n[crash] fatal signal ");
    put(&[b'0' + (sig as u8 / 10), b'0' + (sig as u8 % 10)]);
    put(b" at address 0x");
    put(&buf[at..]);
    put(b"\n");
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn install_crash_log() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_fatal as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGBUS, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGSEGV, &action, std::ptr::null_mut());
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn launcher_update_command(dir: &Path, command: &str) -> i32 {
    use kl_store::install::{self, LauncherUpdate, Paths};

    let Some(root) = dir.parent() else {
        eprintln!("Launcher update needs a mounted user-storage directory.");
        return 1;
    };
    let paths = Paths::under(root);
    let result = match command {
        "begin" => install::launcher_update_begin(&paths).map(|outcome| match outcome {
            LauncherUpdate::Idle => "idle",
            LauncherUpdate::Trial => "trial",
            LauncherUpdate::Reverted => "reverted",
        }),
        "accept" => install::launcher_update_accept(&paths).map(|changed| if changed { "accepted" } else { "idle" }),
        "revert" => install::launcher_update_revert(&paths).map(|changed| if changed { "reverted" } else { "idle" }),
        _ => {
            eprintln!("Expected launcher update command begin, accept or revert.");
            return 2;
        }
    };
    match result {
        Ok(outcome) => {
            println!("{outcome}");
            0
        }
        Err(error) => {
            eprintln!("Launcher update: {error}");
            1
        }
    }
}

fn store_launch_descriptor(dir: &Path, documents: &Path, apps: &[App], id: &str) -> std::io::Result<String> {
    let filename = store_open::installed_entry(dir, documents, id)?;
    let entry = documents.join(&filename);
    apps.iter().find(|app| {
        app.id == filename && !app.background
            && matches!(&app.launch, Launch::Script { path, .. } if Path::new(path) == entry)
    })
    .and_then(App::next_file)
    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Open this app from the launcher grid."))
}

fn main() {
    let mut dir = env_dir("KL_DIR", "/mnt/us/launcher");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--launcher-update") {
        std::process::exit(launcher_update_command(&dir, args.next().as_deref().unwrap_or("")));
    }
    install_crash_log();
    let started = Instant::now();
    // Started from the copy on /var/local (the cable was in at boot): when /mnt/us is back, this same launcher reads its
    // settings, apps and extensions from /mnt/us.
    let mut from_mirror = dir.starts_with("/var/local");
    let run = env_dir("KL_RUN", "/var/tmp/kl");
    let documents = env_dir("KL_DOCUMENTS", "/mnt/us/documents");
    let _ = fs::create_dir_all(&run);
    log(&format!("start v{} dir {} run {}", env!("CARGO_PKG_VERSION"), dir.display(), run.display()));

    let mut settings_path = dir.join("settings.txt");
    let mut settings = Settings::load(&settings_path);
    // Battery use: the screen is on at start (the launcher is running), and the day's totals are kept in usage.txt.
    let mut usage_rec = usage::Recorder::load(&dir, now_secs());
    usage_rec.screen_on(now_secs());
    // The owner's screen timeout goes to the Kindle's own power setting at start, and is kept alive while it is never.
    device::set_screen_timeout(settings.screen_timeout);
    let mut keep_awake_at = std::time::Instant::now();
    // With automatic rotation the saved turn can be stale (it is only saved on manual changes): read the
    // device's turn now, so the first screen is drawn the right way up.
    if settings.auto_rotation {
        if let Some(turn) = device::accelerometer_turn() {
            settings.rotation = turn;
        }
    }
    input::TURN.store(settings.rotation.turn(), Ordering::Relaxed);
    let _ = write_atomic(&run.join("orientation"), settings.rotation.letter());
    if std::env::args().any(|a| a == "--wait-screen") {
        wait_screen(&dir, &run, &settings);
        return;
    }

    if std::env::args().any(|a| a == "--sleep-screen") {
        sleep_screen(&dir, &settings);
        return;
    }

    let wallpapers = wallpaper::load(&dir, settings.sleep_screen_launcher);
    log(&format!("wallpapers at start: home {:?}, sleep {:?}, startup {:?}, {} pictures", wallpapers.home, wallpapers.sleep, wallpapers.startup, wallpapers.images.len()));
    settings.sleep_screen_launcher = wallpapers.sleep != WallpaperChoice::Default;

    // Reviewable system changes recover in Store with its storage write lease.
    let mut recovery_problem = None;
    if let Some(root) = documents.parent().filter(|root| root.is_dir() && !from_mirror && device::usb_state() != kl_core::Usb::Drive) {
        match kl_store::install::recover(&kl_store::install::Paths::under(root)) {
            Ok(Some(what)) => log(&format!("store: {what}")),
            Ok(None) => {}
            Err(e) => recovery_problem = Some(format!("Store recovery pending. {e}")),
        }
    }
    let (config, apps, extensions, mut problems) = load_library(&dir, &documents);
    if let Some(problem) = recovery_problem {
        problems.insert(0, problem);
    }
    let layout = Layout::new(W, H, config.columns, config.rows);

    let mut renderer = match Renderer::new(W, H, &font_paths(&dir, &run)) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("renderer: {e}"));
            std::process::exit(2);
        }
    };
    problems.extend(kl_render::load_icons(&mut renderer, &apps, &layout, Some(&dir.join("cache"))));
    let mut sleep_rotation = wallpaper::refresh_images(&dir, &run, &wallpapers, &mut renderer);
    for p in &problems {
        log(&format!("problem: {p}"));
    }
    log(&format!("{} apps; ready in {} ms", apps.len(), started.elapsed().as_millis()));

    let installed_apps = (!from_mirror && device::usb_state() != kl_core::Usb::Drive)
        .then(|| management::installed_apps(&dir, &apps)).flatten();
    let mut ui = Ui::new(apps, layout);

    ui.version = env!("CARGO_PKG_VERSION").to_string();
    ui.firmware = device::store_device().map(|device| {
        device.firmware.nums.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
    });
    set_device_identity(&mut ui, &settings, &dir);
    ui.device_name = settings.device_name.clone();
    ui.series = device::version_text().and_then(|text| kl_core::models::series_number(&text));
    ui.mac = device::mac_address();
    ui.panel = device::panel_code();
    ui.set_installed_apps(installed_apps);
    ui.set_icon_choices(png_files(&dir.join("icons")));
    ui.set_wallpapers(wallpapers);
    ui.set_extensions(extensions);
    ui.store_update_count = bootstrap::available_updates(&dir);
    ui.store_daily_check = settings.store_daily_check;
    for app in ui.apps() {
        log(&format!("app {}: {}", app.id, app.name));
    }
    ui.notice = problems.first().cloned();
    let mut display = Display::open();
    let mut status = device::status(&settings);
    ui.set_battery(status.battery, status.charging);
    let now = now_secs();
    let mut power = power::Runtime::new(&run, &dir, now, !from_mirror && status.usb != kl_core::Usb::Drive);
    power.observe(now, status.power);
    power.observe_light(&status);
    power.populate(&mut status, now);

    let update_trial = std::env::var("KL_LAUNCHER_UPDATE_TRIAL").as_deref() == Ok("1");
    // Started again over a running app: the app keeps the screen, and a Store request waits for the home screen.
    let attach = system_layer::attaching(&run);
    let store_request = if attach { Ok(None) } else { store_open::take_request(&run) };
    match store_request {
        Ok(Some(id)) if !from_mirror && status.usb != kl_core::Usb::Drive && !update_trial => {
            match store_launch_descriptor(&dir, &documents, ui.apps(), &id)
                .and_then(|next| write_atomic(&run.join("next"), &next))
            {
                Ok(()) => {
                    log(&format!("opening {id} from the Store"));
                    std::process::exit(EXIT_LAUNCH);
                }
                Err(error) => ui.notice = Some(format!("Could not open the app from the Store. {error}")),
            }
        }
        Ok(Some(_)) if update_trial => ui.notice = Some("You can open this app from the grid after the launcher update.".into()),
        Ok(Some(_)) => ui.notice = Some("Eject the Kindle drive before opening the app.".into()),
        Ok(None) => {}
        Err(error) => ui.notice = Some(format!("Could not read the Store's request. {error}")),
    }

    let (tx, rx) = channel();
    match input::find(profile::ACTIVE.touch_name) {
        Some(path) => {
            let t = tx.clone();
            thread::spawn(move || input::touch_loop(path, t));
        }
        None => log("no touch device found"),
    }
    match input::find("gpio-keys") {
        Some(path) => {
            let t = tx.clone();
            thread::spawn(move || input::button_loop(path, t));
        }
        None => log("no page-button device found"),
    }
    {
        let t = tx.clone();
        thread::spawn(move || device::power_loop(t));
    }
    // Holding both page-turn buttons for two seconds restarts the launcher (after confirmation).
    let origin = std::time::Instant::now();
    let mut page_chord = kl_core::page_chord::PageChord::default();
    {
        // The USB state, each second: an event is sent only when it changes.
        let t = tx.clone();
        thread::spawn(move || {
            let mut last = None;
            loop {
                let now = device::usb_state();
                if last != Some(now) {
                    last = Some(now);
                    if t.send(Ev::Usb(now)).is_err() {
                        return;
                    }
                    // Drive mode: the stock system draws its own USB screen soon after; draw ours over it again.
                    if now == kl_core::Usb::Drive {
                        for secs in [2, 3] {
                            thread::sleep(Duration::from_secs(secs));
                            if t.send(Ev::UsbRedraw).is_err() {
                                return;
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_secs(1));
            }
        });
    }
    if status.display_support.auto_rotation {
        let t = tx.clone();
        let start = settings.rotation;
        thread::spawn(move || device::orientation_loop(t, start));
    }
    {
        let t = tx.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            if t.send(Ev::Tick).is_err() {
                return;
            }
        });
    }

    if attach {
        display.set_paused(true);
        log("system layer: started again over the running app");
    } else {
        let first = renderer.draw(&ui, &status);
        display.show(first, &settings, true);
        log(&format!("first screen after {} ms", started.elapsed().as_millis()));
    }
    let mut bootstrapping_store = None;
    let mut mark_trial_ready = update_trial;
    let mut transfer_session = None;
    let mut clock_sync = None::<clock_sync::ClockSync>;
    // The system layer: an app runs under the launcher, which keeps sleep, the lock screen and quick settings.
    let system_layer_on = system_layer::enabled();
    let mut app_session: Option<system_layer::AppSession> = None;
    if attach {
        app_session = Some(system_layer::AppSession::new());
        system_layer::watch(run.clone(), tx.clone());
    }

    loop {
        if mark_trial_ready && display.has_presented_frame() {
            let path = run.join("launcher-update-ready");
            let pid = std::process::id().to_string();
            if write_atomic(&path, &pid).is_ok() {
                mark_trial_ready = false;
            }
        }
        let ev = match page_chord.deadline() {
            Some(at) => match rx.recv_timeout(std::time::Duration::from_millis(at.saturating_sub(origin.elapsed().as_millis() as u64))) {
                Ok(ev) => ev,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ev::ChordTick,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => {
                let Ok(ev) = rx.recv() else { break };
                ev
            }
        };
        let previous_screen = ui.screen;
        let previous_usb = ui.usb;
        let action = match ev {
            Ev::PageKey { code, down } => {
                page_chord.key(code, down, origin.elapsed().as_millis() as u64);
                Action::None
            }
            Ev::ChordTick => {
                if page_chord.tick(origin.elapsed().as_millis() as u64) {
                    let _ = tx.send(Ev::Input(Input::RestartHold));
                }
                Action::None
            }
            Ev::Wifi(info) => {
                ui.set_wifi(info);
                // The Wi-Fi state (connected or not) is read again, so the rows show what the scan shows.
                status = device::status(&settings);
                Action::Redraw { full: false }
            }
            Ev::WifiResult { ok, message } => {
                ui.set_join_result(ok, message);
                status = device::status(&settings);
                Action::Redraw { full: false }
            }
            Ev::StoreBootstrapped(result) => {
                let requested_from = bootstrapping_store.take();
                match result {
                    Err(error) => {
                        log(&format!("Store bootstrap failed: {error}"));
                        ui.notice = Some(format!("Could not install Store. {error}"));
                        Action::Redraw { full: false }
                    }
                    Ok(()) => {
                        let (_, apps, extensions, _) = load_library(&dir, &documents);
                        let app = apps.iter().find(|app| app.id == bootstrap::ENTRY && !app.background).cloned();
                        ui.set_installed_apps(management::installed_apps(&dir, &apps));
                        ui.set_apps(apps);
                        ui.set_extensions(extensions);
                        ui.store_update_count = bootstrap::available_updates(&dir);
                        for problem in kl_render::load_icons(&mut renderer, ui.apps(), &layout, Some(&dir.join("cache"))) {
                            log(&format!("problem: {problem}"));
                        }
                        ui.notice = Some("Store is ready.".into());
                        match (requested_from, app) {
                            (Some(screen), Some(app))
                                if ui.screen == screen && ui.usb != kl_core::Usb::Drive && !app.hidden =>
                            {
                                Action::Launch(app)
                            }
                            _ => Action::Redraw { full: false },
                        }
                    }
                }
            }
            Ev::StoreInstalled { .. } | Ev::StoreRefreshed { .. } | Ev::StoreProgress { .. } => Action::None,
            Ev::ManagedAppsChanged { mut lease, storage_held, message } => {
                ui.installed_busy = false;
                if storage_held {
                    let (_, apps, extensions, _) = load_library(&dir, &documents);
                    ui.set_installed_apps(management::installed_apps(&dir, &apps));
                    ui.set_apps(apps);
                    ui.set_extensions(extensions);
                    ui.store_update_count = bootstrap::available_updates(&dir);
                    for problem in kl_render::load_icons(&mut renderer, ui.apps(), &ui.layout, Some(&dir.join("cache"))) {
                        log(&format!("problem: {problem}"));
                    }
                    if let Err(error) = lease.release() {
                        log(&format!("launcher storage release: {error}"));
                    }
                }
                power.return_storage_lease(lease);
                ui.notice = message;
                Action::Redraw { full: false }
            }
            Ev::Notice(message) => {
                ui.notice = Some(message);
                status = device::status(&settings);
                Action::Redraw { full: false }
            }
            Ev::Background { name, line, ok } => {
                log(&format!("background end {name}: ok={ok} {line}"));
                ui.background_done();
                let state = if ok { "done" } else { "failed" };
                ui.notice = Some(if line.is_empty() { format!("{name}: {state}") } else { format!("{name}: {line}") });
                if ui.screen == Screen::Sleep {
                    Action::None
                } else {
                    Action::Redraw { full: false }
                }
            }
            Ev::Turn(rotation) => {
                if settings.auto_rotation && rotation != settings.rotation {
                    settings.rotation = rotation;
                    input::TURN.store(rotation.turn(), Ordering::Relaxed);
                    let _ = write_atomic(&run.join("orientation"), rotation.letter());
                    log(&format!("screen turned: {}", rotation.letter()));
                    status = device::status(&settings);
                    Action::Redraw { full: true }
                } else {
                    Action::None
                }
            }
            Ev::Tick => {
                let now = now_secs();
                let _ = write_atomic(&run.join("heartbeat"), &now.to_string());
                let app_pid = std::fs::read_to_string(run.join("app.pid")).ok().and_then(|text| text.trim().parse::<i32>().ok());
                usage_rec.sample_cpu(app_pid);
                usage_rec.save_if_due(now);
                status.usage = usage::summary_lines(usage_rec.log(), now);
                // "Never" keeps the screen on: the Kindle's idle timer is refreshed about once a minute (not while charging).
                if settings.screen_timeout == 0 && keep_awake_at.elapsed() >= Duration::from_secs(60) {
                    keep_awake_at = std::time::Instant::now();
                    device::refresh_screen_timeout();
                }
                let clock_sync_result = clock_sync.as_mut().and_then(clock_sync::ClockSync::poll);
                if let Some(result) = clock_sync_result {
                    clock_sync = None;
                    ui.clock_syncing = false;
                    if result.is_ok() {
                        power.break_history();
                    }
                    if ui.screen == Screen::Page(Page::DateTime) {
                        ui.notice = Some(result.err().unwrap_or("Clock synchronized.").into());
                    }
                }
                // Screens with the status bar re-read battery, charging, Wi-Fi and the clock every tick (5 s) and
                // redraw only when the bar would show something new.
                let shown = matches!(ui.screen, Screen::Home | Screen::QuickSettings | Screen::AppMenu | Screen::ConfirmKindle);
                let settings_shown = matches!(ui.screen, Screen::Settings | Screen::Page(_) | Screen::NetworkDetail | Screen::Password | Screen::NetworkName | Screen::SshPassword);
                if shown || settings_shown {
                    let fresh = device::status(&settings);
                    let history_changed = power.observe(now, fresh.power);
                    power.observe_light(&fresh);
                    power.save_if_due(&dir, !from_mirror && fresh.usb != kl_core::Usb::Drive && bootstrapping_store.is_none());
                    // Quick settings also shows CPU, memory and storage: redraw it when those change.
                    let changed = if shown {
                        status.bar_differs(&fresh) || (ui.screen == Screen::QuickSettings
                            && (status.system != fresh.system || status.display_differs(&fresh)))
                    } else {
                        status.settings_differs(&fresh) || (ui.screen == Screen::Page(Page::Battery)
                            && (status.power != fresh.power || history_changed))
                            || (ui.screen == Screen::Page(Page::DateTime) && (clock_sync_result.is_some()
                                || status.clock != fresh.clock || status.date != fresh.date
                                || status.clock_year != fresh.clock_year || status.utc_offset != fresh.utc_offset
                                || status.clock_sync_available != fresh.clock_sync_available))
                    };
                    let warned = ui.set_battery(fresh.battery, fresh.charging);
                    status = fresh;
                    if changed || warned {
                        Action::Redraw { full: false }
                    } else {
                        Action::None
                    }
                } else {
                    Action::None
                }
            }
            Ev::UsbRedraw => {
                if ui.usb == kl_core::Usb::Drive {
                    Action::Redraw { full: true }
                } else {
                    Action::None
                }
            }
            Ev::Usb(usb) => {
                if usb == kl_core::Usb::Drive && clock_sync.take().is_some() {
                    ui.clock_syncing = false;
                    ui.notice = Some("Eject the Kindle drive to sync the clock.".into());
                }
                if usb == ui.usb {
                    Action::None
                } else {
                    log(&format!("usb: {usb:?}"));
                    if usb == kl_core::Usb::Drive {
                        ui.finish_wallpaper_preview();
                    }
                    if ui.usb == kl_core::Usb::Drive && from_mirror && Path::new("/mnt/us/launcher/apps.toml").exists() {
                        // Ejected: /mnt/us is back. This launcher reads its settings, apps and extensions from it again.
                        dir = PathBuf::from("/mnt/us/launcher");
                        settings_path = dir.join("settings.txt");
                        from_mirror = false;
                        settings = Settings::load(&settings_path);
                        power.merge_after_eject(&dir, now_secs());
                        ui.store_daily_check = settings.store_daily_check;
                        let (config, apps, extensions, problems) = load_library(&dir, &documents);
                        ui.set_installed_apps(management::installed_apps(&dir, &apps));
                        ui.set_apps(apps);
                        ui.set_extensions(extensions);
                        ui.set_grid(config.columns, config.rows);
                        ui.store_update_count = bootstrap::available_updates(&dir);
                        ui.notice = problems.first().cloned();
                        for problem in kl_render::load_icons(&mut renderer, ui.apps(), &ui.layout, Some(&dir.join("cache"))) {
                            log(&format!("problem: {problem}"));
                        }
                        log(&format!("the user storage is back: {} apps read from {}", ui.apps().len(), documents.display()));
                    }
                    if ui.usb == kl_core::Usb::Drive && usb != kl_core::Usb::Drive && !from_mirror {
                        let legacy_sleep = Settings::load(&settings_path).sleep_screen_launcher;
                        let wallpapers = wallpaper::load(&dir, legacy_sleep);
                        settings.sleep_screen_launcher = wallpapers.sleep != WallpaperChoice::Default;
                        sleep_rotation = wallpaper::refresh_images(&dir, &run, &wallpapers, &mut renderer);
                        ui.set_wallpapers(wallpapers);
                    }
                    if ui.screen != Screen::Sleep {
                        status = device::status(&settings);
                        power.observe(now_secs(), status.power);
                        ui.set_battery(status.battery, status.charging);
                    }
                    ui.usb = usb;
                    status.usb = usb;
                    Action::Redraw { full: true }
                }
            }
            Ev::AppEnded => {
                if app_session.take().is_some() {
                    usage_rec.app_ended(now_secs());
                    log("system layer: the app ended; the launcher takes the screen back");
                    renderer.set_backdrop(None);
                    input::grab_touch(false);
                    display.set_paused(false);
                    ui.pressed = None;
                    // The app may have been the Store: its refresh and installs change the updates badge.
                    ui.store_update_count = bootstrap::available_updates(&dir);
                    if ui.screen == Screen::Sleep {
                        Action::None
                    } else {
                        ui.screen = Screen::Home;
                        status = device::status(&settings);
                        Action::Redraw { full: true }
                    }
                } else {
                    Action::None
                }
            }
            Ev::Input(input) => {
                match input {
                    Input::Sleep => usage_rec.screen_off(now_secs()),
                    Input::Wake => usage_rec.screen_on(now_secs()),
                    _ => {}
                }
                // An app is in front: its input is its own, except the swipe that opens quick settings and the power
                // events. For those the app is frozen and its screen saved, and the launcher draws again.
                if let Some(session) = app_session.as_mut().filter(|s| s.in_front()) {
                    let opens = system_layer::opens_quick_settings(input, ui.layout.status().bottom());
                    if !matches!(input, Input::Sleep | Input::RestartHold) && !opens {
                        continue;
                    }
                    system_layer::freeze(&run, true);
                    if opens {
                        input::grab_touch(true);
                    }
                    session.saved = Some(display.snapshot().unwrap_or_default());
                    // Every system pop-up over the app is drawn over the app's screen, not the home screen.
                    renderer.set_backdrop(
                        session.saved.as_deref().filter(|s| !s.is_empty())
                            .map(|s| system_layer::upright(s, settings.rotation, display.fb_turn())),
                    );
                    session.dark_mode_at_open = settings.dark_mode;
                    session.rotation_at_open = settings.rotation;
                    display.set_paused(false);
                    log(&format!("system layer: {} over the app", if opens { "quick settings" } else { "sleep" }));
                }
                if !matches!(input, Input::Sleep | Input::Wake) {
                    log(&format!("input {input:?}"));
                }
                if input == Input::Sleep && ui.screen != Screen::Sleep {
                    power.break_history();
                    if clock_sync.take().is_some() {
                        ui.clock_syncing = false;
                        ui.notice = Some("Clock sync stopped when the Kindle went to sleep.".into());
                    }
                }
                // A key of the keyboard is drawn as pressed before its action runs: the tap is seen at once.
                if ui.press_feedback(input) {
                    display.show(renderer.draw(&ui, &status), &settings, false);
                }
                // Any control under a tap is drawn reversed before its action runs (Settings, buttons, rows, tabs).
                if let Input::Tap(x, y) = input {
                    if let Some(frame) = renderer.press_frame(&ui, &status, x, y) {
                        display.show(frame, &settings, false);
                    }
                }
                let before = ui.screen;
                ui.display_support = status.display_support;
                let action = ui.handle(input);
                if input == Input::Wake && before == Screen::Sleep && ui.usb != kl_core::Usb::Drive
                    && ui.wallpapers.sleep == WallpaperChoice::Rotate
                {
                    let sleep = wallpaper::saved_picture(&dir, WallpaperTarget::Sleep, &ui.wallpapers, &mut sleep_rotation);
                    wallpaper::use_launcher_sleep(&run, sleep.is_some());
                    renderer.set_sleep_image(sleep);
                }
                // A Settings page opens with a fresh status: the About text shows the current address and storage.
                if input == Input::Wake || (matches!(ui.screen, Screen::Page(_)) && ui.screen != before) {
                    status = device::status(&settings);
                    power.observe(now_secs(), status.power);
                    ui.set_battery(status.battery, status.charging);
                }
                // The Wi-Fi page scans when it opens.
                if ui.screen == Screen::Page(kl_core::layout::Page::Wifi) && ui.screen != before {
                    ui.wifi_scanning = true;
                    start_wifi_scan(tx.clone());
                }
                action
            }
        };
        power.populate(&mut status, now_secs());
        let action = match action {
            Action::Device(cmd @ (DeviceCmd::OpenInstalledApps | DeviceCmd::RemoveInstalledApp(_))) => {
                let app = match cmd {
                    DeviceCmd::RemoveInstalledApp(index) => ui.installed_app(index).filter(|app| app.can_remove).cloned(),
                    _ => None,
                };
                let root = documents.parent().filter(|_| !from_mirror && device::usb_state() != kl_core::Usb::Drive);
                ui.notice = match root {
                    None => {
                        ui.set_installed_apps(None);
                        Some("Eject the Kindle drive to manage installed apps.".into())
                    }
                    Some(_) if matches!(cmd, DeviceCmd::RemoveInstalledApp(_)) && app.is_none() => {
                        Some("This app cannot be removed.".into())
                    }
                    Some(root) if app.is_some() || kl_store::install::Paths::under(root).journal.is_file() => {
                        if bootstrapping_store.is_some() || (update_trial && run.join("launcher-update-driver").exists()) {
                            Some("Wait for the current installation to finish.".into())
                        } else if let Some(lease) = power.take_storage_lease() {
                            ui.installed_busy = true;
                            let message = app.as_ref().map_or_else(|| "Checking installed apps…".into(), |app| format!("Removing {}…", app.name));
                            management::start(tx.clone(), root.to_path_buf(), lease, app);
                            Some(message)
                        } else {
                            Some("Open Launcher from the Kindle Library to manage installed apps.".into())
                        }
                    }
                    Some(_) => {
                        let (_, apps, _, _) = load_library(&dir, &documents);
                        ui.set_installed_apps(management::installed_apps(&dir, &apps));
                        None
                    }
                };
                Action::Redraw { full: cmd == DeviceCmd::OpenInstalledApps }
            }
            Action::Device(DeviceCmd::ToggleSleepScreen) => {
                let choice = if ui.wallpapers.sleep == WallpaperChoice::Default {
                    WallpaperChoice::Rotate
                } else {
                    WallpaperChoice::Default
                };
                Action::SetWallpaper {
                    target: WallpaperTarget::Sleep, choice, opacity: ui.wallpapers.opacity(WallpaperTarget::Sleep),
                }
            }
            action => action,
        };
        // Quick settings or the sleep screen closed over an app: put the app's screen back and let it run.
        let action = match app_session.as_mut() {
            Some(session) if !session.in_front() && ui.screen == Screen::Home => {
                let mut picture = session.saved.take().unwrap_or_default();
                let theme_changed = settings.dark_mode != session.dark_mode_at_open;
                // A turn (in Settings) is handled like a theme change: the saved screen no longer fits.
                let turned = settings.rotation != session.rotation_at_open;
                let app_redraws = system_layer::can_redraw(&run);
                // An app that redraws gets its screen back as it will look (dark mode is the screen inverted; a half
                // turn is the panel's memory reversed), so its own redraw, which does not flash, lands on a matching
                // picture. From quick settings only the panel's box changes (a calm update); from Settings, a full
                // page, it is one full update. An app that cannot redraw gets its screen back as it was.
                if app_redraws && !picture.is_empty() {
                    if theme_changed {
                        system_layer::in_other_theme(&mut picture);
                    }
                    if turned {
                        picture.reverse();
                    }
                }
                let theme_changed = theme_changed || turned;
                let restored = !picture.is_empty();
                if restored {
                    display.restore(picture);
                }
                display.set_paused(true);
                input::grab_touch(false);
                ui.pressed = None;
                renderer.set_backdrop(None);
                // The redraw request goes first, while the app is still frozen: the signal is delivered the moment
                // the app runs again, before anything it had queued, so nothing is drawn in the old theme.
                if (theme_changed || !restored) && !system_layer::ask_redraw(&run) && !restored {
                    log("system layer: the screen could not be saved and the app cannot redraw");
                }
                system_layer::freeze(&run, false);
                log("system layer: back to the app");
                Action::None
            }
            _ => action,
        };
        if ui.wallpaper_preview.is_none() {
            renderer.set_wallpaper_preview(None);
        }
        let transfer_screen = Screen::Page(kl_core::layout::Page::FileTransfer);
        if ui.screen != transfer_screen || ui.usb == kl_core::Usb::Drive {
            transfer_session = None;
            ui.file_transfer = None;
        } else if previous_screen != ui.screen || previous_usb == kl_core::Usb::Drive {
            match device::wlan_ip().parse() {
                Ok(ip) => match transfer::Session::start(ip, documents.clone()) {
                    Ok(session) => {
                        ui.file_transfer = Some((session.address.clone(), session.code.clone()));
                        transfer_session = Some(session);
                    }
                    Err(error) => ui.notice = Some(format!("File transfer could not start. {error}")),
                },
                Err(_) => ui.notice = Some("Connect to Wi-Fi, then reopen File transfer.".into()),
            }
        }
        // Keep the receiver in scope for as long as the page is open.
        let _session = transfer_session.as_ref();
        match action {
            Action::None => {}
            // Going to sleep draws nothing unless there is something to show: a battery warning, or a picture
            // for the launcher's own sleep screen. Otherwise the Kindle's screensaver (or the last screen) stays.
            Action::Redraw { .. }
                if ui.screen == Screen::Sleep
                    && ui.battery_message().is_none()
                    && !(settings.sleep_screen_launcher && renderer.has_sleep_image()) => {}
            Action::Redraw { full } => {
                if full {
                    status = device::status(&settings);
                }
                display.show(renderer.draw(&ui, &status), &settings, full);
            }
            Action::Launch(mut app) => {
                // The supervisor still runs an app (one closing, or one it started again): a second must wait.
                if system_layer_on && system_layer::app_running(&run) {
                    log(&format!("system layer: {} not opened, an app is still running", app.name));
                    ui.pressed = None;
                    ui.notice = Some(if app_session.is_some() {
                        // Settings over an open app: one app runs at a time.
                        format!("Close the open app first, then open {}.", app.name)
                    } else {
                        "An app is still closing. Try again in a moment.".into()
                    });
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if bootstrapping_store.is_some() {
                    ui.pressed = None;
                    ui.notice = Some("Store is being installed. Please wait.".into());
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if app.launch == Launch::Store || app.id == bootstrap::ENTRY {
                    status = device::status(&settings);
                    if let Some(entry) = bootstrap::entry_for(app, &documents) {
                        app = entry;
                    } else {
                        ui.pressed = None;
                        ui.notice = Some(if from_mirror || status.usb == kl_core::Usb::Drive {
                            "Eject the Kindle drive to install Store.".into()
                        } else if !status.wifi_on || status.wifi_state != "connected" {
                            "Connect to Wi-Fi to install Store.".into()
                        } else {
                            bootstrapping_store = Some(ui.screen);
                            bootstrap::start(tx.clone(), documents.clone());
                            "Preparing Store…".into()
                        });
                        display.show(renderer.draw(&ui, &status), &settings, false);
                        continue;
                    }
                }
                let pressed = renderer.draw(&ui, &status);
                display.show(pressed.clone(), &settings, false);
                let Some(next) = app.next_file() else {
                    ui.pressed = None;
                    ui.notice = Some(format!("{}: cannot be started", app.name));
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                };
                if let Err(e) = write_atomic(&run.join("next"), &next) {
                    ui.pressed = None;
                    ui.notice = Some(format!("cannot write {}: {e}", run.join("next").display()));
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                log(&format!("launch {} ({})", app.name, app.id));
                // The opening zoom (Settings > Opening animation): a card grows from the icon to the screen in
                // three fast steps.
                if let Some(icon) = ui.pressed.filter(|_| settings.animation) {
                    for step in kl_core::layout::zoom_steps(icon, ui.layout.screen(), 3) {
                        let mut frame = pressed.clone();
                        let radius = (step.w.min(step.h) as f32 * 0.12).min(48.0);
                        let area = (step.x, step.y, step.w, step.h);
                        kl_render::image::card(&mut frame, W as usize, area, radius, 4.0, settings.dark_mode);
                        display.show(frame, &settings, false);
                        thread::sleep(Duration::from_millis(70));
                    }
                }
                // The system layer: the supervisor runs the app and the launcher stays, its drawing paused.
                if system_layer_on && system_layer::request_launch(&run) {
                    log("system layer: the supervisor runs the app; the launcher stays");
                    app_session = Some(system_layer::AppSession::new());
                    usage_rec.app_started(&usage::app_name(&run), now_secs());
                    display.set_paused(true);
                    ui.pressed = None;
                    system_layer::watch(run.clone(), tx.clone());
                    continue;
                }
                std::process::exit(EXIT_LAUNCH);
            }
            Action::SetHidden(changes) => {
                let change = || update_apps_toml(&dir, |config| {
                    for (id, hidden) in &changes { config.set_hidden(id, *hidden); }
                });
                match save_library_setting(from_mirror, &mut power, change) {
                    Ok(()) if ui.screen == Screen::Page(kl_core::layout::Page::AppVisibility) => ui.notice = None,
                    Ok(()) => {}
                    Err(error) => {
                        ui.restore_visibility(&changes);
                        ui.notice = Some(format!("Could not save app visibility. {error}"));
                    }
                }
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetGrid { columns, rows } => {
                let change = || update_apps_toml(&dir, |config| {
                    config.columns = columns;
                    config.rows = rows;
                });
                match save_library_setting(from_mirror, &mut power, change) {
                    Ok(()) => {
                        ui.set_grid(columns, rows);
                        for problem in kl_render::load_icons(&mut renderer, ui.apps(), &ui.layout, Some(&dir.join("cache"))) {
                            log(&format!("icon: {problem}"));
                        }
                    }
                    Err(error) => ui.notice = Some(format!("Could not save the home screen layout. {error}")),
                }
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::RunBackground(app) => {
                run_background(app, tx.clone());
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::ScanWifi => {
                start_wifi_scan(tx.clone());
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::JoinWifi { name, secured, password, saved } => {
                start_wifi_join(tx.clone(), name, secured, password, saved);
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::ForgetWifi(name) => {
                start_wifi_forget(tx.clone(), name);
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetDeviceName(name) => {
                ui.notice = Some(match save_device_name(&name, &settings_path, &mut settings) {
                    Ok(()) => {
                        ui.device_name = Some(name);
                        "Device name saved".to_string()
                    }
                    Err(error) => format!("The name could not be saved. {error}"),
                });
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetSshPassword(password) => {
                start_ssh_job(tx.clone(), move |dir| match device::set_ssh_password(dir, &password) {
                    Ok(()) => "SSH password set".to_string(),
                    Err(e) => format!("SSH password: {e}"),
                });
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::CheckSsh => {
                start_ssh_job(tx.clone(), |dir| format!("SSH: {}", device::check_ssh(dir)));
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetOrder(order) => {
                save_apps_toml(&dir, &mut ui, "order", |c| {
                    for (id, place) in &order {
                        c.set_order(id, *place);
                    }
                });
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetIcon { id, icon } => {
                save_apps_toml(&dir, &mut ui, "icon", |c| c.set_icon(&id, icon.clone()));
                renderer.forget_icon(&id);
                if let Some(app) = ui.apps().iter().find(|a| a.id == id).cloned() {
                    let layout = ui.layout;
                    for problem in kl_render::load_icons(&mut renderer, &[app], &layout, Some(&dir.join("cache"))) {
                        log(&format!("icon: {problem}"));
                        ui.notice = Some(problem);
                    }
                }
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::PreviewWallpaper { target, choice } => {
                let result = save_library_setting(from_mirror, &mut power, || {
                    wallpaper::selected_picture(&dir, target, &choice, &mut 0)
                });
                match result {
                    Ok(image) => renderer.set_wallpaper_preview(image),
                    Err(error) => {
                        renderer.set_wallpaper_preview(None);
                        if let Some(draft) = ui.wallpaper_preview.as_mut() {
                            draft.error = Some(format!("Could not preview wallpaper. {error}"));
                        }
                    }
                }
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::SetWallpaper { target, choice, opacity } => {
                let mut updated = ui.wallpapers.clone();
                updated.set_selection(target, choice);
                updated.set_opacity(target, opacity);
                let mut next = 0;
                let result = if bootstrapping_store.is_some() {
                    Err(std::io::Error::other("Wait for the Store to finish installing."))
                } else {
                    save_library_setting(from_mirror, &mut power, || {
                        let image = wallpaper::prepared_picture(&dir, target, &updated, &mut next)?;
                        write_atomic(&wallpaper::preferences_path(&dir), &wallpaper::preferences(&updated))?;
                        Ok(image)
                    })
                };
                match result {
                    Ok(image) => {
                        ui.finish_wallpaper_preview();
                        ui.set_wallpapers(updated);
                        renderer.set_wallpaper_preview(None);
                        ui.notice = None;
                        match target {
                            WallpaperTarget::Home => renderer.set_home_wallpaper(image),
                            WallpaperTarget::Sleep => {
                                sleep_rotation = next;
                                settings.sleep_screen_launcher = ui.wallpapers.sleep != WallpaperChoice::Default;
                                status.sleep_launcher = settings.sleep_screen_launcher;
                                wallpaper::use_launcher_sleep(&run, image.is_some());
                                if ui.wallpapers.sleep == WallpaperChoice::Rotate && image.is_none() {
                                    ui.notice = Some("Copy PNGs to launcher/screensavers to use rotation.".into());
                                }
                                renderer.set_sleep_image(image);
                            }
                            WallpaperTarget::Startup => {}
                        }
                    }
                    Err(error) => ui.notice = Some(format!("Could not save wallpaper. {error}")),
                }
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            Action::KindleMode => {
                log("leaving for the Kindle UI");
                wallpaper::use_launcher_sleep(&run, false);
                // The dialog turns into "Opening the Kindle UI"; the wait screen fills its blocks from here.
                let (frame, area) = renderer.waiting_dialog(&ui.layout, 0, settings.dark_mode);
                if app_session.is_some() {
                    // Over an app the rest of the screen is the app's picture, and the app is closing: the dialog
                    // goes on a plain page, or the app would seem to stay open for the minute the Kindle UI takes.
                    display.show(frame, &settings, true);
                } else {
                    display.show_region(frame, area, &settings);
                }
                std::process::exit(EXIT_KINDLE_UI);
            }
            Action::Device(cmd) => {
                if cmd == DeviceCmd::RefreshTimeZone {
                    device::refresh_clock(&mut status);
                    ui.notice = Some(if status.utc_offset.is_some() {
                        "Time zone read from Kindle."
                    } else {
                        "Could not read the Kindle’s time zone."
                    }.into());
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if cmd == DeviceCmd::SyncClock {
                    let problem = if from_mirror || device::usb_state() == kl_core::Usb::Drive {
                        Some("Eject the Kindle drive before syncing the clock.")
                    } else if bootstrapping_store.is_some() || (update_trial && run.join("launcher-update-driver").exists()) {
                        Some("Wait for installation to finish before syncing the clock.")
                    } else if !status.clock_sync_available {
                        Some("Clock sync is unavailable on this firmware.")
                    } else if !status.wifi_on || status.wifi_state != "connected" {
                        Some("Connect to Wi-Fi before syncing the clock.")
                    } else {
                        None
                    };
                    if let Some(problem) = problem {
                        ui.notice = Some(problem.into());
                    } else if clock_sync.is_none() {
                        match clock_sync::ClockSync::start() {
                            Ok(job) => {
                                clock_sync = Some(job);
                                ui.clock_syncing = true;
                            }
                            Err(error) => {
                                log(&format!("clock sync: {error}"));
                                ui.notice = Some("Could not start clock sync.".into());
                            }
                        }
                    }
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if let DeviceCmd::PickModel(index) = cmd {
                    ui.notice = Some(match apply_device_model(index, &settings_path, &mut settings) {
                        Ok(name) => {
                            set_device_identity(&mut ui, &settings, &dir);
                            format!("Kindle model: {name}")
                        }
                        Err(error) => format!("The model could not be saved. {error}"),
                    });
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if matches!(cmd, DeviceCmd::StoreToggleDailyCheck | DeviceCmd::StoreClearCache) {
                    ui.notice = if from_mirror || device::usb_state() == kl_core::Usb::Drive {
                        Some("Eject the Kindle drive before changing Store settings.".into())
                    } else if bootstrapping_store.is_some() {
                        Some("Wait for the Store to finish installing.".into())
                    } else {
                        match apply_store_setting(cmd, &dir, &settings_path, &mut settings) {
                            Ok(()) => {
                                ui.store_daily_check = settings.store_daily_check;
                                if cmd == DeviceCmd::StoreClearCache {
                                    ui.store_update_count = 0;
                                    Some("Cleared the saved store catalog.".into())
                                } else {
                                    None
                                }
                            }
                            Err(error) => Some(format!("The Store settings could not be changed. {error}")),
                        }
                    };
                    display.show(renderer.draw(&ui, &status), &settings, cmd == DeviceCmd::StoreClearCache);
                    continue;
                }
                if cmd == DeviceCmd::ToggleLowPower {
                    let mut updated = settings.clone();
                    updated.low_power = !updated.low_power;
                    ui.notice = if from_mirror || ui.usb == kl_core::Usb::Drive {
                        Some("Eject the Kindle drive before changing Low power mode.".into())
                    } else if bootstrapping_store.is_some() {
                        Some("Wait for the Store to finish installing.".into())
                    } else {
                        match power.write_leased(&Settings::preferences_path(&settings_path), &updated.preferences_text()) {
                            Ok(()) => {
                                settings = updated;
                                if settings.low_power && !status.display_support.front_light {
                                    None
                                } else {
                                    power.set_light_mode(settings.low_power).err().map(|error| {
                                        let mode = if settings.low_power { "on" } else { "off" };
                                        format!("Low power mode is {mode}. {error}")
                                    })
                                }
                            }
                            Err(error) => Some(format!("Could not save Low power mode. {error}")),
                        }
                    };
                    status = device::status(&settings);
                    power.populate(&mut status, now_secs());
                    display.show(renderer.draw(&ui, &status), &settings, false);
                    continue;
                }
                if matches!(cmd, DeviceCmd::ToggleLargeText | DeviceCmd::ToggleHighContrast | DeviceCmd::ToggleAnimation | DeviceCmd::ToggleDarkMode | DeviceCmd::CycleScreenTimeout) {
                    let mut updated = settings.clone();
                    match cmd {
                        DeviceCmd::ToggleLargeText => updated.large_text = !updated.large_text,
                        DeviceCmd::ToggleHighContrast => updated.high_contrast = !updated.high_contrast,
                        DeviceCmd::ToggleAnimation => updated.animation = !updated.animation,
                        DeviceCmd::ToggleDarkMode => updated.dark_mode = !updated.dark_mode,
                        DeviceCmd::CycleScreenTimeout => updated.screen_timeout = kl_core::settings::next_screen_timeout(updated.screen_timeout),
                        _ => unreachable!(),
                    }
                    ui.notice = if from_mirror {
                        Some("Eject the Kindle drive before changing appearance settings.".into())
                    } else if bootstrapping_store.is_some() {
                        Some("Wait for the Store to finish installing.".into())
                    } else {
                        let result = if matches!(cmd, DeviceCmd::ToggleLargeText | DeviceCmd::ToggleHighContrast | DeviceCmd::CycleScreenTimeout) {
                            power.write_leased(&Settings::preferences_path(&settings_path), &updated.preferences_text())
                        } else {
                            power.write_leased(&settings_path, &updated.base_text())
                        };
                        match result {
                            Ok(()) => {
                                settings = updated;
                                status.large_text = settings.large_text;
                                status.high_contrast = settings.high_contrast;
                                status.animation = settings.animation;
                                status.dark_mode = settings.dark_mode;
                                status.screen_timeout = settings.screen_timeout;
                                if cmd == DeviceCmd::CycleScreenTimeout {
                                    device::set_screen_timeout(settings.screen_timeout);
                                }
                                None
                            }
                            Err(error) => Some(format!("Could not save appearance settings. {error}")),
                        }
                    };
                    // Over an app that follows the theme, the app under the panel is shown in the new theme at once
                    // (its picture inverted), so this one update shows the whole screen as it will be.
                    if cmd == DeviceCmd::ToggleDarkMode {
                        if let Some(session) = app_session.as_ref() {
                            if let Some(saved) = session.saved.as_deref().filter(|s| !s.is_empty()) {
                                let mut picture = system_layer::upright(saved, settings.rotation, display.fb_turn());
                                if settings.dark_mode != session.dark_mode_at_open && system_layer::can_redraw(&run) {
                                    system_layer::in_other_theme(&mut picture);
                                }
                                renderer.set_backdrop(Some(picture));
                            }
                        }
                    }
                    display.show(renderer.draw(&ui, &status), &settings, cmd != DeviceCmd::ToggleAnimation);
                    continue;
                }
                // Power off: the screen says so, and stays that way once the Kindle is off (e-ink keeps its picture).
                if cmd == DeviceCmd::PowerOff {
                    let frame = renderer.message("Powered Off", "Press the power button to turn the Kindle on again.");
                    display.show(frame, &settings, true);
                    // Let the screen finish its refresh before the system halts.
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
                let previous_rotation = settings.rotation;
                if matches!(cmd, DeviceCmd::Light(_) | DeviceCmd::SetLight(_)) && status.display_support.front_light {
                    power.manually_changed_light();
                }
                run_device(cmd, &mut status, &mut settings, &dir, &run, &settings_path);
                power.populate(&mut status, now_secs());
                // The Kindle is going off: keep the "Powered Off" picture, do not draw the screen again.
                if cmd == DeviceCmd::PowerOff {
                    continue;
                }
                // Wi-Fi turned on from the Wi-Fi page: look for networks once it is up.
                if cmd == DeviceCmd::ToggleWifi && status.wifi_on && ui.screen == Screen::Page(kl_core::layout::Page::Wifi) {
                    ui.wifi_scanning = true;
                    start_wifi_scan_after(tx.clone(), 4);
                }
                let full = settings.rotation != previous_rotation;
                display.show(renderer.draw(&ui, &status), &settings, full);
            }
        }
    }
}

/// `kl-launcher --wait-screen`: run by the supervisor while the stock Kindle UI starts (about a minute) and
/// stopped by it once the library is ready. It draws only the "Opening the Kindle UI" dialog, over what is on the
/// screen (the grid the launcher left), and fills one block of its bar every 1.5 s with a small partial update.
fn wait_screen(dir: &Path, run: &Path, settings: &Settings) {
    let mut renderer = match Renderer::new(W, H, &font_paths(dir, run)) {
        Ok(r) => r,
        Err(e) => return log(&format!("wait screen: {e}")),
    };
    log("wait screen");
    let layout = Layout::new(W, H, kl_core::config::DEFAULT_COLUMNS, kl_core::config::DEFAULT_ROWS);
    let mut display = Display::open();
    let mut step = 0;
    loop {
        let (frame, area) = renderer.waiting_dialog(&layout, step, settings.dark_mode);
        display.show_region(frame, area, settings);
        thread::sleep(Duration::from_millis(1500));
        step += 1;
    }
}

/// Draw the launcher's sleep picture once and return. The supervisor runs this when the Kindle goes to sleep while an
/// app is open: the picture stays on the screen after the power goes off.
fn sleep_screen(dir: &Path, settings: &Settings) {
    let wallpapers = wallpaper::load(dir, settings.sleep_screen_launcher);
    let Some(image) = wallpaper::saved_picture(dir, WallpaperTarget::Sleep, &wallpapers, &mut 0) else {
        return log("sleep screen: no picture chosen");
    };
    log("sleep screen");
    let mut display = Display::open();
    display.show(image.pixels, settings, true);
}

/// Run an app without leaving the launcher (apps.toml `mode = "background"`, KUAL `exitmenu: false`), with the
/// same environment the supervisor gives apps. Its output is collected and the last line sent back when it ends.
fn run_background(app: App, tx: Sender<Ev>) {
    let args: Vec<String> = match &app.launch {
        Launch::Script { path, hooks: false } => vec![path.clone()],
        Launch::Script { path, hooks: true } => vec!["-c".into(), ". \"$1\"; on_run".into(), "sh".into(), path.clone()],
        Launch::Shell { command } => vec!["-c".into(), command.clone()],
        _ => return,
    };
    log(&format!("background start {} ({})", app.name, app.id));
    thread::spawn(move || {
        let result = Command::new("/bin/sh")
            .args(&args)
            .current_dir(&app.dir)
            .env("PRIMITIVE_LAUNCHER", "1")
            .env("PRIMITIVE_FRAMEWORK", "stopped")
            .stdin(Stdio::null())
            .output();
        let (ok, line) = match result {
            Ok(out) => {
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                (out.status.success(), last)
            }
            Err(e) => (false, e.to_string()),
        };
        let _ = tx.send(Ev::Background { name: app.name, line, ok });
    });
}

/// Apply `change` to apps.toml and write it back. A file that is not valid TOML is never overwritten.
fn save_apps_toml(dir: &Path, ui: &mut Ui, what: &str, change: impl FnOnce(&mut Config)) {
    match update_apps_toml(dir, change) {
        Ok(()) => log(&format!("apps.toml: {what} saved")),
        Err(error) => ui.notice = Some(format!("Could not save {what}. {error}")),
    }
}

fn update_apps_toml(dir: &Path, change: impl FnOnce(&mut Config)) -> std::io::Result<()> {
    let path = dir.join("apps.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut config = Config::load(&text).map_err(|_| std::io::Error::new(
        std::io::ErrorKind::InvalidData, "There is an error in apps.toml.",
    ))?;
    change(&mut config);
    write_atomic(&path, &config.to_toml())
}

fn save_library_setting<T>(
    from_mirror: bool,
    power: &mut power::Runtime,
    change: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    if from_mirror {
        return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "Eject the Kindle drive and try again."));
    }
    power.with_storage(change)
}

/// The PNG files available as app icons, sorted.
fn png_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("._")))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    files.sort();
    files
}

/// The model in use and how it is known: detected from the codename, else the owner's choice, else unknown. Shown on
/// the Device page and written to `device.txt` for the Store. The serial is never read or written.
fn set_device_identity(ui: &mut Ui, settings: &Settings, dir: &Path) {
    let detected = device::detected_model();
    let (model, source) = match (detected, settings.device_model.as_deref().and_then(kl_core::models::model_by_id)) {
        (Some(model), _) => (Some(model), "detected"),
        (None, Some(model)) => (Some(model), "selected"),
        (None, None) => (None, "unknown"),
    };
    let codename = device::platform_codename().unwrap_or_else(|| "none".into());
    let arch = device::store_device().map_or_else(|| "armhf".to_string(), |device| device.arch);
    let firmware = ui.firmware.clone().unwrap_or_else(|| "unknown".into());
    let name = model.map_or("Unknown Kindle", |model| model.name);
    let untested = model.is_none_or(|model| !model.verified);
    ui.device_model_id = model.map(|model| model.id.to_string());
    ui.device_model_name = name.to_string();
    ui.device_source = source.to_string();
    ui.device_untested = untested;
    ui.device_profile = Some(name.to_string());
    let text = format!(
        "model={}\nmodel_name={name}\nmodel_source={source}\ncodename={codename}\narch={arch}\nfirmware={firmware}\nuntested={}\n",
        model.map_or("unknown", |model| model.id),
        if untested { "yes" } else { "no" },
    );
    if let Err(error) = write_atomic(&dir.join("device.txt"), &text) {
        log(&format!("could not write device.txt: {error}"));
    }
}

/// Save the owner's device name in the preferences file.
fn save_device_name(name: &str, settings_path: &Path, settings: &mut Settings) -> std::io::Result<()> {
    if !kl_core::ui::valid_device_name(name) {
        return Err(std::io::Error::other("use 1 to 32 letters, digits or spaces"));
    }
    let mut updated = settings.clone();
    updated.device_name = Some(name.to_string());
    write_atomic(&Settings::preferences_path(settings_path), &updated.preferences_text())?;
    *settings = updated;
    Ok(())
}

/// Save the owner's model (`models::MODELS[index]`) in the preferences file, and return its name.
fn apply_device_model(index: u8, settings_path: &Path, settings: &mut Settings) -> std::io::Result<&'static str> {
    let model = kl_core::models::MODELS
        .get(index as usize)
        .ok_or_else(|| std::io::Error::other("unknown model"))?;
    let mut updated = settings.clone();
    updated.device_model = Some(model.id.to_string());
    write_atomic(&Settings::preferences_path(settings_path), &updated.preferences_text())?;
    *settings = updated;
    Ok(model.name)
}

fn apply_store_setting(cmd: DeviceCmd, launcher: &Path, settings_path: &Path, settings: &mut Settings) -> std::io::Result<()> {
    if cmd == DeviceCmd::StoreClearCache {
        return kl_store::refresh::clear_cache(launcher);
    }
    let mut updated = settings.clone();
    updated.store_daily_check = !settings.store_daily_check;
    write_atomic(settings_path, &updated.base_text())?;
    *settings = updated;
    Ok(())
}

fn run_device(cmd: DeviceCmd, status: &mut Status, settings: &mut Settings, dir: &Path, run: &Path, settings_path: &Path) {
    match cmd {
        DeviceCmd::Light(step) => device::set_light((status.light as i32 + step).clamp(0, device::LEVEL_MAX as i32) as u32),
        DeviceCmd::Warmth(step) => device::set_warmth((status.warmth as i32 + step).clamp(0, device::LEVEL_MAX as i32) as u32),
        DeviceCmd::ToggleWifi => device::set_wifi(!status.wifi_on),
        DeviceCmd::ToggleSsh => device::set_ssh(&dir.to_string_lossy(), !status.ssh),
        DeviceCmd::ToggleDns => device::set_public_dns(&dir.to_string_lossy(), !status.public_dns),
        DeviceCmd::Restart => device::system_command("reboot"),
        DeviceCmd::PowerOff => device::system_command("halt -f -p"),
        DeviceCmd::RestartLauncher => device::restart_launcher(),
        // Handled with the other saved settings, where the change is written and applied.
        DeviceCmd::CycleScreenTimeout => {}
        DeviceCmd::QuitAppAndRestart => {
            system_layer::end_app(run);
            device::restart_launcher();
        }
        DeviceCmd::StoreRefresh
        | DeviceCmd::StoreRemove(_)
        | DeviceCmd::StoreOpen(_)
        | DeviceCmd::OpenInstalledApps
        | DeviceCmd::RemoveInstalledApp(_)
        | DeviceCmd::StoreInstall(_)
        | DeviceCmd::StoreUpdateAll
        | DeviceCmd::StoreToggleDailyCheck
        | DeviceCmd::ToggleLowPower
        | DeviceCmd::StoreClearCache
        | DeviceCmd::ToggleLargeText
        | DeviceCmd::ToggleHighContrast
        | DeviceCmd::ToggleAnimation
        | DeviceCmd::ToggleDarkMode
        | DeviceCmd::ToggleSleepScreen => {}
        DeviceCmd::SyncClock | DeviceCmd::RefreshTimeZone => {}
        // Handled where the settings are saved: the model choice writes the preferences and the device file.
        DeviceCmd::PickModel(_) => {}
        DeviceCmd::ToggleAutostart => device::set_autostart(&dir.to_string_lossy(), !status.autostart),
        DeviceCmd::SetLight(level) => device::set_light(level),
        DeviceCmd::SetWarmth(level) => device::set_warmth(level),
        DeviceCmd::ToggleAutoRotation => {
            // Quick settings' Auto-rotate: the same as Settings > Screen > Auto, from the device's turn.
            settings.auto_rotation = !settings.auto_rotation;
            if settings.auto_rotation {
                if let Some(turn) = device::accelerometer_turn() {
                    settings.rotation = turn;
                    input::TURN.store(turn.turn(), Ordering::Relaxed);
                    let _ = write_atomic(&run.join("orientation"), turn.letter());
                }
            }
            if let Err(e) = write_atomic(settings_path, &settings.base_text()) {
                log(&format!("settings: {e}"));
            }
        }
        DeviceCmd::ToggleRotation => {
            settings.auto_rotation = false;
            settings.rotation = settings.rotation.flipped();
            input::TURN.store(settings.rotation.turn(), Ordering::Relaxed);
            if let Err(e) = write_atomic(settings_path, &settings.base_text()) {
                log(&format!("settings: {e}"));
            }
            let _ = write_atomic(&run.join("orientation"), settings.rotation.letter());
        }
    }
    *status = device::status(settings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kl_core::IconSource;

    fn shell(name: &str, command: &str) -> App {
        App {
            id: name.into(),
            name: name.into(),
            icon: IconSource::None,
            launch: Launch::Shell { command: command.into() },
            dir: "/".into(),
            order: None,
            hidden: false,
            background: true,
        }
    }

    fn finished(rx: &std::sync::mpsc::Receiver<Ev>) -> (String, String, bool) {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ev::Background { name, line, ok }) => (name, line, ok),
            _ => panic!("no background result"),
        }
    }

    #[test]
    fn store_settings_are_shared_and_cache_clear_preserves_install_data() {
        let root = std::env::temp_dir().join(format!("kl-launcher-store-settings-{}", std::process::id()));
        let store = root.join("store");
        let settings_path = root.join("settings.txt");
        let cache = store.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("index.json"), "saved catalog").unwrap();
        fs::write(store.join("installed.json"), "installed records").unwrap();
        let mut settings = Settings::default();
        assert!(settings.store_daily_check);
        apply_store_setting(DeviceCmd::StoreToggleDailyCheck, &root, &settings_path, &mut settings).unwrap();
        let stored = Settings::parse(&fs::read_to_string(&settings_path).unwrap());
        assert!(!settings.store_daily_check);
        assert!(!stored.store_daily_check);
        for _ in 0..2 {
            apply_store_setting(DeviceCmd::StoreClearCache, &root, &settings_path, &mut settings).unwrap();
            assert!(!cache.exists());
            assert_eq!(fs::read_to_string(store.join("installed.json")).unwrap(), "installed records");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_store_request_uses_the_installed_apps_normal_launcher_entry() {
        let root = std::env::temp_dir().join(format!("kl-launcher-store-open-{}", std::process::id()));
        let documents = root.join("documents");
        let dir = root.join("launcher");
        fs::create_dir_all(&documents).unwrap();
        let filename = documents.join("Example App.sh");
        fs::write(&filename, "#!/bin/sh\n# Name: Example App\n").unwrap();
        let app = kl_store::record::Installed {
            id: "example-app".into(), files: vec!["documents/Example App.sh".into()], ..Default::default()
        };
        let mut record = kl_store::record::Record::new();
        record.apps.insert(app.id.clone(), app);
        kl_store::record::save(&dir.join("store/installed.json"), &record).unwrap();
        let mut apps = kl_core::apps::scriptlets(&documents);
        let next = store_launch_descriptor(&dir, &documents, &apps, "example-app").unwrap();
        assert_eq!(next, format!("{}\nscript\n{}\n", documents.display(), filename.display()));
        assert!(store_launch_descriptor(&dir, &documents, &apps, "uninstalled-app").is_err());
        apps[0].launch = Launch::Shell { command: "echo ignored".into() };
        assert!(store_launch_descriptor(&dir, &documents, &apps, "example-app").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_background_app_reports_its_last_output_line() {
        let (tx, rx) = channel();
        run_background(shell("Echo", "echo one; echo two; echo; echo \"$PRIMITIVE_LAUNCHER\""), tx);
        assert_eq!(finished(&rx), ("Echo".into(), "1".into(), true));
    }

    #[test]
    fn a_failing_background_app_is_reported_as_failed() {
        let (tx, rx) = channel();
        run_background(shell("Fail", "echo trouble >&2; exit 3"), tx);
        assert_eq!(finished(&rx), ("Fail".into(), "trouble".into(), false));
    }
}
