//! Desktop driver: draws the launcher's screens to PNG files for review, with the same core and renderer
//! as the Kindle build. No device calls.
//!
//!   kl-desktop --documents DIR [--config apps.toml] [--fonts DIR] [--wallpaper FILE.png] --out DIR
//!   kl-desktop --launcher-icon FILE.png [--fonts DIR]
//!
//! Screens written: home (each page), pressed tile, settings, confirm, sleep.

use std::{fs, path::{Path, PathBuf}};

use kl_core::{
    discover, Action, Config, Input, Layout, Rotation, Screen, Status, Ui,
    layout::{Page, Rect, Row},
    wallpaper::{WallpaperChoice, WallpaperSettings, WallpaperTarget},
};
use kl_render::Renderer;

const W: u32 = 1264;
const H: u32 = 1680;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn write_png(path: &PathBuf, w: u32, h: u32, gray: &[u8]) {
    let file = fs::File::create(path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(gray).unwrap();
    println!("wrote {}", path.display());
}

fn fonts(dir: &str) -> Vec<String> {
    ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"].iter().map(|f| format!("{dir}/{f}")).collect()
}

fn demo_status() -> Status {
    use kl_core::battery::{Reading, Sample, Source};
    let now = 1_800_000_000;
    Status {
        clock: "10:42".into(),
        sound_level: None,
        media: None,
        date: "Tue 17 Nov".into(),
        clock_year: Some(2026),
        utc_offset: Some(7 * 3600),
        clock_sync_available: true,
        battery: Some(78),
        charging: false,
        power: Reading { percent: Some(78), source: Source::OnBattery },
        battery_now: now,
        battery_history: [(19, 91, Source::OnBattery, true), (15, 74, Source::OnBattery, false),
            (10, 67, Source::Charging, true), (8, 91, Source::Charging, false),
            (4, 86, Source::OnBattery, false), (0, 78, Source::OnBattery, false)]
            .into_iter().map(|(hours, percent, source, gap_before)| Sample {
                observed_at: now - hours * 3600, percent, source, gap_before,
            }).collect(),
        wifi_on: true,
        wifi_state: "connected".into(),
        wifi_signal: Some((4, 5)),
        light: 12,
        warmth: 6,
        ssh: false,
        ssh_address: String::new(),
        ssh_password_set: true,
        usb: kl_core::Usb::Unplugged,
        public_dns: false,
        autostart: true,
        sleep_launcher: false,
        auto_rotation: true,
        dark_mode: false,
        animation: true,
        rotation: Rotation::Upright,
        memory: "MemAvailable 312 MB".into(),
        ip: "192.0.2.28".into(),
        storage: "5.0 GB free".into(),
        system: primitive_quick::Stats { cpu: Some(12), ram: Some((180, 512)), storage: Some((1_100_000_000, 6_100_000_000)) },
        ..Status::default()
    }
}

fn show_wallpaper_demo(renderer: &mut Renderer, ui: &mut Ui, status: &Status, out: &Path, path: &Path) {
    let png = fs::read(path).expect("read wallpaper PNG");
    let filename = path.file_name().expect("wallpaper filename").to_string_lossy().into_owned();
    let choice = WallpaperChoice::File(filename.clone());
    let home = kl_render::image::wallpaper_tile(&png, W, H).expect("decode Home wallpaper");
    let fitted = kl_render::image::picture_tile(&png, W, H).expect("decode Sleep and Startup wallpaper");
    let capture = |renderer: &mut Renderer, ui: &Ui, theme: &Status, name: &str| {
        write_png(&out.join(format!("{name}.png")), W, H, &renderer.draw(ui, theme));
    };
    let tap = |r: Rect| Input::Tap(r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let row = |ui: &Ui, page, label| ui.layout.page_sections(page).into_iter()
        .flat_map(|section| section.rows).find(|item| item.row == label).expect("wallpaper settings row").rect;

    ui.set_wallpapers(WallpaperSettings { images: vec![filename], ..WallpaperSettings::default() });
    ui.screen = Screen::Settings;
    let wallpaper = ui.layout.settings_home_rows(0).into_iter()
        .find(|&(page, _)| page == Page::Wallpaper).expect("Wallpaper settings row").1;
    ui.handle(tap(wallpaper));
    assert_eq!(ui.screen, Screen::Page(Page::Wallpaper));

    for (target, picture, name) in [
        (WallpaperTarget::Home, &home, "settings-wallpaper-preview-home"),
        (WallpaperTarget::Sleep, &fitted, "settings-wallpaper-preview-sleep"),
        (WallpaperTarget::Startup, &fitted, "settings-wallpaper-preview-startup"),
    ] {
        let picker = Screen::WallpaperPicker(target);
        ui.handle(tap(row(ui, Page::Wallpaper, Row::Wallpaper(target))));
        assert_eq!(ui.screen, picker);
        if target == WallpaperTarget::Sleep {
            capture(renderer, ui, status, "settings-wallpaper-picker");
        }
        let index = ui.wallpapers.choices(target).position(|entry| entry == choice).expect("sample wallpaper in picker");
        let photo = tap(ui.layout.visibility_row(index, ui.wallpaper_scroll));
        let preview = Action::PreviewWallpaper { target, choice: choice.clone() };
        assert_eq!(ui.handle(photo), preview);
        assert_eq!(ui.screen, Screen::WallpaperPreview);
        assert_eq!(ui.wallpapers.selection(target), &WallpaperChoice::Default);
        assert_eq!(ui.wallpaper_preview.as_ref().unwrap().opacity, 100);
        renderer.set_wallpaper_preview(Some(picture.clone()));
        capture(renderer, ui, status, name);

        let controls = ui.layout.wallpaper_preview();
        let opacity = if target == WallpaperTarget::Home {
            let dark = Status { dark_mode: true, ..status.clone() };
            capture(renderer, ui, &dark, "settings-wallpaper-preview-home-dark");
            ui.handle(tap(controls.opacity_track));
            assert_eq!(ui.wallpaper_preview.as_ref().unwrap().opacity, 50);
            ui.handle(tap(controls.opacity_down));
            assert_eq!(ui.wallpaper_preview.as_ref().unwrap().opacity, 40);
            ui.handle(tap(controls.opacity_up));
            assert_eq!(ui.wallpaper_preview.as_ref().unwrap().opacity, 50);
            capture(renderer, ui, status, "settings-wallpaper-preview-home-opacity");

            let dismiss = ui.handle(tap(ui.layout.done_button()));
            assert!(!matches!(dismiss, Action::SetWallpaper { .. }));
            renderer.set_wallpaper_preview(None);
            assert_eq!(ui.screen, picker);
            assert!(ui.wallpaper_preview.is_none());
            assert_eq!(ui.wallpapers.selection(target), &WallpaperChoice::Default);
            assert_eq!(ui.wallpapers.opacity(target), 100);

            assert_eq!(ui.handle(photo), preview);
            assert_eq!(ui.wallpaper_preview.as_ref().unwrap().opacity, 100);
            renderer.set_wallpaper_preview(Some(home.clone()));
            ui.handle(tap(controls.opacity_track));
            50
        } else {
            100
        };

        assert_eq!(ui.handle(tap(controls.apply)), Action::SetWallpaper {
            target, choice: choice.clone(), opacity,
        });
        assert_eq!(ui.wallpapers.selection(target), &WallpaperChoice::Default);
        assert_eq!(ui.wallpapers.opacity(target), 100);
        // Simulate the successful save callback after Apply. Only this step changes the saved choices.
        let mut saved = ui.wallpapers.clone();
        saved.set_selection(target, choice.clone());
        saved.set_opacity(target, opacity);
        ui.set_wallpapers(saved);
        ui.finish_wallpaper_preview();
        assert_eq!(ui.screen, picker);
        assert!(ui.wallpaper_preview.is_none());
        assert_eq!(ui.wallpapers.selection(target), &choice);
        assert_eq!(ui.wallpapers.opacity(target), opacity);
        renderer.set_wallpaper_preview(None);
        ui.handle(tap(ui.layout.done_button()));
        assert_eq!(ui.screen, Screen::Page(Page::Wallpaper));
    }

    ui.handle(Input::PageBack);
    ui.handle(Input::PageBack);
    ui.handle(tap(ui.layout.done_button()));
    assert_eq!(ui.screen, Screen::Home);

    renderer.set_home_wallpaper(Some(home));
    capture(renderer, ui, status, "home-wallpaper");
    capture(renderer, ui, &Status { dark_mode: true, ..status.clone() }, "dark-home-wallpaper");
    let configured = ui.wallpapers.clone();
    let mut full_opacity = configured.clone();
    full_opacity.set_opacity(WallpaperTarget::Home, 100);
    ui.set_wallpapers(full_opacity);
    capture(renderer, ui, status, "home-wallpaper-full-opacity");
    ui.set_wallpapers(configured);
    renderer.set_home_wallpaper(None);

    write_png(&out.join("startup-wallpaper.png"), W, H, &fitted.pixels);
    renderer.set_sleep_image(Some(fitted));
    ui.handle(Input::Sleep);
    capture(renderer, ui, status, "sleep-wallpaper");
    renderer.set_sleep_image(None);
    ui.handle(Input::Wake);
    assert_eq!(ui.screen, Screen::Home);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let font_dir = arg(&args, "--fonts").unwrap_or_else(|| "assets/fonts".into());
    if let Some(path) = arg(&args, "--launcher-icon") {
        // The Primitive logo goes in the centre tile.
        let logo = fs::read("assets/primitive-icon.png").ok().and_then(|b| kl_render::image::decode_png(&b).ok());
        let mut r = Renderer::new(600, 920, &fonts(&font_dir)).expect("fonts");
        let gray = r.launcher_icon(logo.as_ref());
        write_png(&PathBuf::from(path), 600, 920, &gray);
        return;
    }
    let documents = PathBuf::from(arg(&args, "--documents").expect("--documents DIR"));
    let out = PathBuf::from(arg(&args, "--out").expect("--out DIR"));
    fs::create_dir_all(&out).unwrap();
    let (config, mut problems) = match arg(&args, "--config") {
        Some(p) => Config::parse(&fs::read_to_string(&p).unwrap_or_default()),
        None => (Config::default(), Vec::new()),
    };
    let (apps, more) = discover(&documents, &config);
    problems.extend(more);
    let extensions_root = documents.parent().map(|p| p.join("extensions")).unwrap_or_default();
    let (extensions, more) = kl_core::kual::discover_root(&extensions_root, kl_core::kual::OASIS_3_MODEL);
    problems.extend(more);
    let layout = Layout::new(W, H, config.columns, config.rows);
    let mut renderer = Renderer::new(W, H, &fonts(&font_dir)).expect("fonts");
    problems.extend(kl_render::load_icons(&mut renderer, &apps, &layout, None));
    for p in &problems {
        eprintln!("problem: {p}");
    }
    let mut ui = Ui::new(apps, layout);
    ui.version = env!("CARGO_PKG_VERSION").to_string();
    ui.firmware = Some("5.18.2".into());
    ui.device_profile = Some("Kindle Oasis 3".into());
    ui.set_installed_apps(Some([
        ("primitive-files", "Files", "0.1.2", true),
        ("primitive-notes", "Notes", "0.1.1", true),
        ("primitive-reader", "Reader", "0.1.0", true),
        ("primitive-store", "Store", "0.1.7", false),
    ].into_iter().map(|(id, name, version, can_remove)| kl_core::ui::InstalledApp {
        id: id.into(),
        name: name.into(),
        version: version.into(),
        can_remove,
    }).collect()));
    ui.set_extensions(extensions);
    let wallpaper_path = arg(&args, "--wallpaper").map(PathBuf::from);
    let status = Status { sleep_launcher: wallpaper_path.is_some(), ..demo_status() };
    let shot = |r: &mut Renderer, ui: &Ui, name: &str| {
        let t = std::time::Instant::now();
        let gray = r.draw(ui, &status);
        eprintln!("{name}: drawn in {} ms", t.elapsed().as_millis());
        write_png(&out.join(format!("{name}.png")), W, H, &gray);
    };
    if let Some(path) = wallpaper_path {
        show_wallpaper_demo(&mut renderer, &mut ui, &status, &out, &path);
    }
    for page in 0..ui.pages() {
        ui.page = page;
        shot(&mut renderer, &ui, &format!("home-{}", page + 1));
    }
    ui.page = 0;
    if ui.apps().iter().any(|app| app.launch == kl_core::Launch::Store) {
        ui.store_update_count = 2;
        shot(&mut renderer, &ui, "home-store-updates");
        ui.store_update_count = 0;
        ui.notice = Some("Downloading Store…".into());
        shot(&mut renderer, &ui, "home-store-bootstrap");
    }
    ui.notice = problems.first().cloned();
    if ui.notice.is_some() {
        shot(&mut renderer, &ui, "home-notice");
        ui.notice = None;
    }
    // Low battery: the footer message and the status strip must agree.
    let mut low = demo_status();
    low.battery = Some(9);
    ui.set_battery(low.battery, low.charging);
    let gray = renderer.draw(&ui, &low);
    write_png(&out.join("home-low-battery.png"), W, H, &gray);
    ui.set_battery(Some(90), false);
    // Status icons: charging with Wi-Fi on but not connected, and full with Wi-Fi off.
    let mut charging = demo_status();
    charging.battery = Some(37);
    charging.charging = true;
    charging.wifi_state = "not connected".into();
    write_png(&out.join("status-charging.png"), W, H, &renderer.draw(&ui, &charging));
    let mut full = demo_status();
    full.battery = Some(100);
    full.wifi_on = false;
    full.wifi_state = "off".into();
    write_png(&out.join("status-wifi-off.png"), W, H, &renderer.draw(&ui, &full));
    ui.pressed = Some(ui.layout.icon(1));
    shot(&mut renderer, &ui, "home-pressed");
    ui.pressed = None;
    ui.screen = kl_core::Screen::Settings;
    shot(&mut renderer, &ui, "settings");
    // Demo networks for the Wi-Fi page (made-up names).
    let net = |name: &str, level: u32, secured: bool, known: bool| kl_core::wifi::Network { name: name.into(), level, max: 5, secured, known };
    // A long list (made-up names) to check scrolling: 30 networks, mixed signal, locks and saved ones.
    let mut networks: Vec<kl_core::wifi::Network> = (1..=30)
        .map(|i| net(&format!("Network {i:02}"), 1 + (i * 7) % 5, i % 3 != 0, i % 5 == 0))
        .collect();
    networks.push(net("Office 5G", 2, true, true));
    let demo_wifi = kl_core::wifi::WifiInfo { current: Some(net("Home Network", 4, true, true)), networks };
    ui.set_wifi(demo_wifi);
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Wifi);
    shot(&mut renderer, &ui, "wifi-long-list");
    let off = Status { wifi_on: false, wifi_state: "off".into(), ..demo_status() };
    let saved = std::mem::take(&mut ui.wifi);
    write_png(&out.join("wifi-off.png"), W, H, &renderer.draw(&ui, &off));
    ui.wifi = saved;
    for _ in 0..3 {
        ui.handle(Input::Swipe { dx: 0.0, dy: -400.0, start_y: 900.0 });
    }
    shot(&mut renderer, &ui, "wifi-long-list-scrolled");
    ui.wifi_scroll = 0;
    ui.wifi_target = Some(net("Cafe Corner", 4, true, false));
    ui.screen = kl_core::Screen::NetworkDetail;
    shot(&mut renderer, &ui, "network-detail");
    ui.wifi_target = Some(net("Home Network", 4, true, true));
    shot(&mut renderer, &ui, "network-detail-saved");
    ui.wifi_target = Some(net("Cafe Corner", 4, true, false));
    ui.screen = kl_core::Screen::Password;
    ui.password.text = "Sunny day".chars().collect();
    ui.password.caret = ui.password.text.len();
    shot(&mut renderer, &ui, "password");
    ui.show_password = true;
    shot(&mut renderer, &ui, "password-shown");
    ui.show_password = false;
    ui.password = primitive_keyboard::Editor::new(63);
    // A network that is not in the list: its name, then the password screen with the open-network hint.
    ui.screen = kl_core::Screen::NetworkName;
    ui.network_name.text = "Garden Shed".chars().collect();
    ui.network_name.caret = ui.network_name.text.len();
    shot(&mut renderer, &ui, "network-name");
    ui.wifi_hidden = true;
    ui.wifi_target = Some(net("Garden Shed", 0, true, false));
    ui.screen = kl_core::Screen::Password;
    shot(&mut renderer, &ui, "password-hidden");
    ui.wifi_hidden = false;
    ui.network_name = primitive_keyboard::Editor::new(32);
    // Bluetooth (made-up names and placeholder addresses, never a real device): the page with devices, one device
    // paired and connected, a nearby one, the pairing confirmation, the quick settings and the status bar with both icons.
    let bt = |address: &str, name: &str, paired: bool, connected: bool, rssi: Option<i32>| kl_core::bluetooth::Device {
        name: name.into(),
        address: kl_core::bluetooth::BdAddr::parse(address).expect("demo address"),
        paired,
        connected,
        discovered: !paired,
        rssi,
        class: Some(0x0434),
    };
    let demo_bt = kl_core::bluetooth::BtInfo {
        on: true,
        devices: vec![
            bt("AA:BB:CC:DD:EE:02", "Headphones X", true, true, Some(-40)),
            bt("AA:BB:CC:DD:EE:03", "Speaker Y", true, false, None),
            bt("AA:BB:CC:DD:EE:04", "Earbuds Z", false, false, Some(-70)),
        ],
    };
    ui.set_bluetooth(demo_bt);
    let bt_on = Status { bluetooth_on: true, bluetooth_connected: Some("Headphones X".into()), ..demo_status() };
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Bluetooth);
    write_png(&out.join("bluetooth-devices.png"), W, H, &renderer.draw(&ui, &bt_on));
    ui.bt_target = kl_core::bluetooth::BdAddr::parse("AA:BB:CC:DD:EE:02");
    ui.screen = kl_core::Screen::BluetoothDevice;
    write_png(&out.join("bluetooth-device-connected.png"), W, H, &renderer.draw(&ui, &bt_on));
    ui.bt_target = kl_core::bluetooth::BdAddr::parse("AA:BB:CC:DD:EE:04");
    write_png(&out.join("bluetooth-device-nearby.png"), W, H, &renderer.draw(&ui, &bt_on));
    ui.ask_bluetooth_confirmation("417 902".into());
    write_png(&out.join("bluetooth-confirm.png"), W, H, &renderer.draw(&ui, &bt_on));
    ui.screen = kl_core::Screen::QuickSettings;
    write_png(&out.join("quick-settings-bluetooth.png"), W, H, &renderer.draw(&ui, &bt_on));
    // With headphones connected, the sound row shows the level (1 to 8).
    ui.set_sound(Some(4));
    let with_sound = Status { sound_level: Some(4), ..bt_on.clone() };
    write_png(&out.join("quick-settings-sound.png"), W, H, &renderer.draw(&ui, &with_sound));
    // A session from an app that plays: its card shows the track and the controls.
    let session = kl_core::media::MediaSession {
        app: "music".into(),
        name: "Music".into(),
        playing: true,
        title: "Evening Bells".into(),
        position: 83,
        length: 203,
        pid: Some(1),
        ..Default::default()
    };
    ui.set_media(Some(session.clone()));
    let with_media = Status { sound_level: Some(4), media: Some(session), ..bt_on.clone() };
    write_png(&out.join("quick-settings-media.png"), W, H, &renderer.draw(&ui, &with_media));
    ui.set_media(None);
    ui.set_sound(None);
    ui.screen = kl_core::Screen::Home;
    let both = Status { bluetooth_on: true, bluetooth_connected: Some("Headphones X".into()), ssh: true, ..demo_status() };
    write_png(&out.join("status-bluetooth-ssh.png"), W, H, &renderer.draw(&ui, &both));
    // Without SSH, Bluetooth takes the slot next to Wi-Fi.
    let bt_only = Status { bluetooth_on: true, bluetooth_connected: Some("Headphones X".into()), ..demo_status() };
    write_png(&out.join("status-bluetooth.png"), W, H, &renderer.draw(&ui, &bt_only));
    ui.set_bluetooth(kl_core::bluetooth::BtInfo::default());
    ui.bt_target = None;
    ui.screen = kl_core::Screen::Settings;
    let swipe = |dy| Input::Swipe { dx: 0.0, dy, start_y: H as f32 / 2.0 };
    assert_eq!(ui.settings_scroll, 0);
    let bottom = ui.layout.settings_scroll_max();
    assert!(bottom > 0, "the Settings overview needs to scroll at Oasis size");
    ui.handle(swipe(-((bottom + 80) as f32)));
    assert_eq!(ui.settings_scroll, bottom);
    shot(&mut renderer, &ui, "settings-scrolled");
    let date_time = kl_core::layout::Page::DateTime;
    let (_, row) = ui.layout.settings_home_rows(bottom).into_iter()
        .find(|(page, _)| *page == date_time).expect("Date and time row");
    ui.handle(Input::Tap((row.x + row.w / 2) as f32, (row.y + row.h / 2) as f32));
    assert_eq!(ui.screen, kl_core::Screen::Page(date_time));
    ui.handle(Input::PageBack);
    assert_eq!(ui.screen, kl_core::Screen::Settings);
    assert_eq!(ui.settings_scroll, bottom, "Back restores the Settings position");
    ui.handle(swipe((bottom + 80) as f32));
    assert_eq!(ui.settings_scroll, 0);
    for page in kl_core::layout::Page::ALL {
        ui.screen = kl_core::Screen::Page(page);
        if page == kl_core::layout::Page::Wifi {
            shot(&mut renderer, &ui, "settings-wi-fi-network");
            continue;
        }
        let name = format!("settings-{}", page.title().to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-"));
        shot(&mut renderer, &ui, &name);
        if page == kl_core::layout::Page::AppVisibility {
            let bottom = ui.visibility_scroll_max();
            if bottom > 0 {
                ui.handle(swipe(-((bottom + 80) as f32)));
                assert_eq!(ui.visibility_scroll, bottom);
                shot(&mut renderer, &ui, "settings-app-visibility-scrolled");
                ui.handle(swipe((bottom + 80) as f32));
                assert_eq!(ui.visibility_scroll, 0);
            }
        }
    }
    ui.screen = kl_core::Screen::Settings;
    ui.screen = kl_core::Screen::ConfirmKindle;
    shot(&mut renderer, &ui, "confirm");
    // Dark mode: the grid, Settings and the dialog, white on black; pictures keep their look.
    let dark = Status { dark_mode: true, ..demo_status() };
    for (screen, name) in [(kl_core::Screen::Home, "dark-home"), (kl_core::Screen::Settings, "dark-settings"), (kl_core::Screen::ConfirmKindle, "dark-confirm")] {
        ui.screen = screen;
        write_png(&out.join(format!("{name}.png")), W, H, &renderer.draw(&ui, &dark));
    }
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::About);
    write_png(&out.join("dark-settings-about.png"), W, H, &renderer.draw(&ui, &dark));
    let turned = kl_render::for_panel(renderer.draw(&ui, &status), Rotation::UpsideDown, 0);
    write_png(&out.join("settings-about-turned.png"), W, H, &turned);
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Battery);
    let no_battery = Status { power: Default::default(), battery_history: Vec::new(), ..demo_status() };
    write_png(&out.join("settings-battery-unavailable.png"), W, H, &renderer.draw(&ui, &no_battery));
    write_png(&out.join("dark-settings-battery.png"), W, H, &renderer.draw(&ui, &Status { low_power: true, ..dark.clone() }));
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Accessibility);
    write_png(&out.join("settings-accessibility-larger.png"), W, H, &renderer.draw(&ui, &Status {
        large_text: true, high_contrast: true, ..demo_status()
    }));
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Display);
    let display_support = kl_core::ui::DisplaySupport { front_light: true, warmth: false, auto_rotation: false };
    write_png(&out.join("settings-display-no-warmth.png"), W, H, &renderer.draw(&ui, &Status { display_support, ..demo_status() }));
    ui.screen = kl_core::Screen::Home;
    let first = ui.layout.cell(0);
    ui.handle(Input::LongTap(first.x as f32 + 5.0, first.y as f32 + 5.0));
    shot(&mut renderer, &ui, "app-menu");
    ui.screen = kl_core::Screen::Home;
    if ui.apps().iter().any(|a| a.id == "extensions") {
        ui.open_extensions();
        shot(&mut renderer, &ui, "extensions");
        if let Some(i) = ui.ext_items().iter().position(|item| item.is_submenu()) {
            let row = ui.layout.list_row(i);
            ui.handle(Input::Tap(row.x as f32 + 10.0, row.y as f32 + 10.0));
            shot(&mut renderer, &ui, "extensions-submenu");
        }
        ui.screen = kl_core::Screen::Home;
    }
    // A critical battery: the only sleep screen with text (otherwise sleep draws nothing, or a picture).
    ui.set_battery(Some(4), false);
    shot(&mut renderer, &ui, "sleep-battery");
    ui.set_battery(Some(90), false);
    // Quick settings over the grid, and the status bar with Wi-Fi off and SSH on.
    ui.screen = kl_core::Screen::Home;
    ui.handle(Input::Swipe { dx: 0.0, dy: 400.0, start_y: 40.0 });
    shot(&mut renderer, &ui, "quick-settings");
    write_png(&out.join("dark-quick-settings.png"), W, H, &renderer.draw(&ui, &Status { dark_mode: true, ..demo_status() }));
    // The SSH icon beside the Wi-Fi icon in the quick settings header.
    write_png(&out.join("quick-settings-ssh.png"), W, H, &renderer.draw(&ui, &Status { ssh: true, ..demo_status() }));
    ui.screen = kl_core::Screen::Home;
    let mut off = demo_status();
    off.wifi_on = false;
    off.ssh = true;
    write_png(&out.join("status-airplane-ssh.png"), W, H, &renderer.draw(&ui, &off));
    // SSH on, with its connect command, public DNS on: the SSH & DNS page. Made-up address.
    let mut ssh_on = demo_status();
    ssh_on.ssh = true;
    ssh_on.ssh_address = "ssh -p 2222 root@192.168.1.40".into();
    ssh_on.public_dns = true;
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Connection);
    write_png(&out.join("ssh-dns-on.png"), W, H, &renderer.draw(&ui, &ssh_on));
    // The result of Check the server, shown at the bottom of the same page (made-up text).
    ui.notice = Some("SSH: server running; port 2222 listening; firewall open".into());
    write_png(&out.join("ssh-dns-check.png"), W, H, &renderer.draw(&ui, &ssh_on));
    ui.notice = None;
    // The SSH password screen, with a password typed (made up).
    ui.screen = kl_core::Screen::SshPassword;
    ui.ssh_password.text = "Sunny-day9".chars().collect();
    ui.ssh_password.caret = ui.ssh_password.text.len();
    write_png(&out.join("ssh-password.png"), W, H, &renderer.draw(&ui, &ssh_on));
    ui.ssh_password = primitive_keyboard::Editor::new(64);
    // A key under the finger: drawn in the on look until its action runs (the shared keyboard look).
    ui.screen = kl_core::Screen::NetworkName;
    ui.network_name.text = "Gard".chars().collect();
    ui.network_name.caret = ui.network_name.text.len();
    let (_, pressed) = ui.layout.keyboard_keys_with(ui.network_name.layer, "Next").into_iter().find(|(d, _)| d.key == primitive_keyboard::Key::Char('e')).unwrap();
    ui.pressed_key = Some(pressed);
    write_png(&out.join("keyboard-pressed.png"), W, H, &renderer.draw(&ui, &ssh_on));
    // The power menu over the grid, and the quick panel with its Power tile.
    ui.screen = kl_core::Screen::PowerMenu;
    write_png(&out.join("power-menu.png"), W, H, &renderer.draw(&ui, &ssh_on));
    // Use the sibling catalog and its drafts by default, or a freshly generated review catalog.
    let review_catalog = arg(&args, "--catalog");
    let catalog = PathBuf::from(review_catalog.as_deref().unwrap_or("../doannt-kindle-catalog"));
    let mut folders = vec![catalog.join("apps")];
    if review_catalog.is_none() {
        folders.push(catalog.join("drafts"));
    }
    let mut entries = Vec::new();
    for folder in folders {
        let read = match fs::read_dir(&folder) {
            Ok(read) => read,
            Err(error) if review_catalog.is_some() => panic!("read {}: {error}", folder.display()),
            Err(_) => continue,
        };
        for e in read.flatten() {
            let dir = e.path();
            let Ok(text) = fs::read_to_string(dir.join("app.json")) else { continue };
            let Ok(listing) = serde_json::from_str::<kl_store::catalog::Listing>(&text) else { continue };
            if let Ok(icon) = fs::read(dir.join(&listing.icon)) {
                let _ = renderer.set_store_icon(&listing.id, &icon);
            }
            for n in 1..=2 {
                if let Ok(bytes) = fs::read(dir.join(format!("shots/{n}.png"))) {
                    let _ = renderer.set_store_shot(&listing.id, n - 1, &bytes);
                }
            }
            entries.push(kl_core::store::StoreEntry::from_listing(&listing, None));
        }
    }
    assert!(review_catalog.is_none() || !entries.is_empty(), "no listings in {}", catalog.display());
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    if !entries.is_empty() {
        use kl_core::store::{Status, StoreTab};
        ui.set_store(entries.clone());
        if let Ok(data) = fs::read(catalog.join("collections.json")) {
            if let Ok(collections) = serde_json::from_slice::<kl_store::catalog::CollectionIndex>(&data) {
                ui.store_collections = collections.collections;
            }
        }
        ui.store_tab = StoreTab::Today;
        ui.screen = kl_core::Screen::Store;
        write_png(&out.join("store-today.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_tab = StoreTab::Apps;
        write_png(&out.join("store-apps.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_tab = StoreTab::Search;
        ui.pressed_key = None;
        ui.store_query.text = "browser".chars().collect();
        ui.store_query.caret = ui.store_query.text.len();
        write_png(&out.join("store-search.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_search_keyboard = true;
        write_png(&out.join("store-search-keyboard.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_search_keyboard = false;
        // Preview update actions on installable entries only.
        let mut with_installs = entries.clone();
        let mut installable = with_installs.iter_mut().filter(|entry| entry.kind == kl_core::store::Kind::Install);
        if let Some(entry) = installable.next() {
            entry.status = Status::Update { installed: "0.2.16".into(), latest: entry.latest.clone().unwrap_or_default() };
        }
        if let Some(entry) = installable.next() {
            entry.status = Status::Installed(entry.latest.clone().unwrap_or_default());
        }
        ui.set_store(with_installs);
        ui.store_tab = StoreTab::Updates;
        write_png(&out.join("store-updates.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_batch_confirm = true;
        write_png(&out.join("store-update-all.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_batch_confirm = false;
        ui.store_settings_open = true;
        write_png(&out.join("store-settings.png"), W, H, &renderer.draw(&ui, &ssh_on));
        ui.store_settings_open = false;
        ui.set_store(entries.clone());
        // One app's page, at the top and scrolled; the installable app first, then the link.
        ui.store_tab = StoreTab::Today;
        if let Some(i) = ui.store.iter().position(|e| e.kind == kl_core::store::Kind::Install) {
            ui.store_open = Some(i);
            ui.screen = kl_core::Screen::StoreApp;
            ui.store_scroll = 0;
            write_png(&out.join("store-app.png"), W, H, &renderer.draw(&ui, &ssh_on));
            ui.store_sheet = Some(i);
            write_png(&out.join("store-install-sheet.png"), W, H, &renderer.draw(&ui, &ssh_on));
            let night = kl_core::Status { dark_mode: true, rotation: Rotation::UpsideDown, ..ssh_on.clone() };
            write_png(&out.join("store-install-sheet-dark.png"), W, H, &renderer.draw(&ui, &night));
            let turned = kl_render::for_panel(renderer.draw(&ui, &night), Rotation::UpsideDown, 0);
            write_png(&out.join("store-install-sheet-turned.png"), W, H, &turned);
            let mut required = ui.store[i].clone();
            required.id = "primitive-hello".into();
            required.name = "Hello".into();
            required.latest = Some("0.1.0".into());
            required.size = Some(2_600_000);
            required.changes = vec!["Changes nothing outside its own folder.".into()];
            ui.store_requirements = Some(kl_core::ui::StoreRequirementReview { apps: vec![required], page: 0 });
            write_png(&out.join("store-required-apps.png"), W, H, &renderer.draw(&ui, &ssh_on));
            ui.store_requirements = None;
            ui.store_progress = Some("Downloading Primitive Browser…".into());
            ui.store_progress_percent = Some(40);
            write_png(&out.join("store-install-progress.png"), W, H, &renderer.draw(&ui, &ssh_on));
            ui.store_sheet = None;
            ui.store_progress = None;
            ui.store_progress_percent = None;
            ui.store_scroll = ui.store_scroll_max().min(900);
            write_png(&out.join("store-app-scrolled.png"), W, H, &renderer.draw(&ui, &ssh_on));
            ui.store_scroll = 0;
            let app = &mut ui.store[i];
            app.status = Status::Installed(app.latest.clone().unwrap_or_default());
            app.launchable = true;
            write_png(&out.join("store-open-installed.png"), W, H, &renderer.draw(&ui, &ssh_on));
        }
        if let Some(i) = ui.store.iter().position(|e| e.kind == kl_core::store::Kind::Link) {
            ui.store_open = Some(i);
            ui.store_scroll = 0;
            write_png(&out.join("store-link.png"), W, H, &renderer.draw(&ui, &ssh_on));
        }
        ui.screen = kl_core::Screen::Home;
    }
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::FileTransfer);
    ui.file_transfer = Some(("http://192.0.2.12:8080".into(), "005628".into()));
    write_png(&out.join("file-transfer.png"), W, H, &renderer.draw(&ui, &ssh_on));
    ui.file_transfer = None;
    ui.screen = kl_core::Screen::Page(kl_core::layout::Page::Store);
    ui.store_clear_confirm = true;
    write_png(&out.join("settings-store-clear.png"), W, H, &renderer.draw(&ui, &ssh_on));
    ui.store_clear_confirm = false;
    ui.screen = kl_core::Screen::Home;
    // The drive-mode screen: shown while the drive is on a computer (no taps then).
    ui.usb = kl_core::Usb::Drive;
    write_png(&out.join("usb-drive.png"), W, H, &renderer.draw(&ui, &ssh_on));
    ui.usb = kl_core::Usb::Unplugged;
    ui.pressed_key = None;
    ui.network_name = primitive_keyboard::Editor::new(32);
    ui.handle(Input::Sleep);
    // The launcher's own sleep screen with a picture (the launcher icon stands in for one).
    match kl_render::sleep_image(std::path::Path::new("assets/launcher-icon.png"), W, H) {
        Ok(picture) => {
            renderer.set_sleep_image(Some(picture));
            shot(&mut renderer, &ui, "sleep-picture");
            renderer.set_sleep_image(None);
        }
        Err(e) => eprintln!("sleep picture: {e}"),
    }
    for step in [0, 4] {
        // The dialog over the grid, as the Kindle shows it.
        ui.screen = kl_core::Screen::Home;
        let mut base = renderer.draw(&ui, &status);
        let (layer, area) = renderer.waiting_dialog(&ui.layout, step, false);
        kl_render::image::copy_rounded(&mut base, &layer, W as usize, (area.x, area.y, area.w, area.h), 38.0);
        write_png(&out.join(format!("waiting-{step}.png")), W, H, &base);
    }
}
