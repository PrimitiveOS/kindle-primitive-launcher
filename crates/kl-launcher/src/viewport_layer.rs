//! Feature-gated system surfaces over a projected foreground frame.

use std::{
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    thread,
};

use kl_core::{
    viewport::{LogicalTurn, ViewportTransform},
    Input, Layout, Rect, Rotation, Screen, Settings, Ui, Usb,
};
use kl_kindle::{
    display::Display,
    input::{ViewportEvent, ViewportGeneration},
    viewport::{
        live_viewport_owner, read_ready_viewport, FramebufferGeometry, PageKeyFence, ReadyViewport,
        ViewportOwner, ViewportRecord,
    },
    Ev,
};
use kl_render::Renderer;

use crate::system_layer;

pub enum Event {
    Shared(Ev),
    Native(ViewportEvent),
}

pub fn input_stream(shared: Receiver<Ev>) -> (Sender<Event>, Receiver<Event>) {
    let (tx, rx) = channel();
    let bridge = tx.clone();
    thread::spawn(move || {
        while let Ok(event) = shared.recv() {
            if bridge.send(Event::Shared(event)).is_err() {
                break;
            }
        }
    });
    (tx, rx)
}

#[derive(Debug, PartialEq, Eq)]
enum Offer {
    Legacy,
    Invalid,
    Granted(ViewportOwner),
}

fn offer(run: &Path) -> Offer {
    let path = run.join("app-viewport-offer");
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Offer::Legacy,
        Err(_) => return Offer::Invalid,
    };
    if !meta.is_file() {
        return Offer::Invalid;
    }
    ViewportOwner::read(&path).map_or(Offer::Invalid, Offer::Granted)
}

trait Panel {
    fn framebuffer(&self) -> Option<FramebufferGeometry>;
    fn owner_stopped(&self, owner: &ViewportOwner) -> bool;
    fn hold_page_keys(&self, owner: &ViewportOwner) -> bool;
    fn resume_page_keys(&self, owner: &ViewportOwner);
    fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport>;
    fn freeze(&mut self, owner: &ViewportOwner, on: bool) -> bool;
    fn snapshot(&self, framebuffer: FramebufferGeometry) -> Option<Vec<u8>>;
    fn restore(&mut self, framebuffer: FramebufferGeometry, pixels: Vec<u8>) -> bool;
    fn redraw_app(&self, owner: &ViewportOwner);
    fn grab_touch(&mut self, on: bool) -> bool;
    fn paint(&mut self, ready: &ReadyViewport, region: Rect, pixels: Vec<u8>) -> bool;
    fn paint_sleep(
        &mut self,
        framebuffer: FramebufferGeometry,
        turn: LogicalTurn,
        pixels: Vec<u8>,
    ) -> bool;
    fn recorded_generation(&self, owner: &ViewportOwner) -> Option<u64>;
    fn minimum(&self, owner: &ViewportOwner) -> u64;
    fn set_minimum(&mut self, owner: &ViewportOwner, generation: Option<u64>) -> bool;
}

pub struct DevicePanel<'a> {
    pub run: &'a Path,
    pub display: &'a mut Display,
}

impl Panel for DevicePanel<'_> {
    fn framebuffer(&self) -> Option<FramebufferGeometry> {
        (kl_kindle::device::usb_state() != Usb::Drive)
            .then(|| self.display.viewport_framebuffer())
            .flatten()
    }

    fn owner_stopped(&self, owner: &ViewportOwner) -> bool {
        live_viewport_owner(self.run, Path::new("/proc"))
            .is_some_and(|live| live.stopped && &live.owner == owner)
    }

    fn hold_page_keys(&self, owner: &ViewportOwner) -> bool {
        self.owner_stopped(owner)
            && PageKeyFence::new(&owner.viewport_path(self.run))
                .hold()
                .is_ok()
    }

    fn resume_page_keys(&self, owner: &ViewportOwner) {
        if let Err(error) = resume_page_keys_for_owner(self.run, owner) {
            kl_kindle::log(&format!(
                "system layer: could not release page keys: {error}"
            ));
        }
    }

    fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport> {
        let Offer::Granted(owner) = offer(self.run) else {
            return None;
        };
        let framebuffer = self.framebuffer()?;
        read_ready_viewport(self.run, Path::new("/proc"), framebuffer, minimum, stopped)
            .filter(|ready| ready.owner == owner)
    }

    fn freeze(&mut self, owner: &ViewportOwner, on: bool) -> bool {
        system_layer::foreground_process(self.run)
            .filter(|p| p.matches_viewport_owner(owner))
            .is_some_and(|p| system_layer::freeze(self.run, p, on))
    }

    fn snapshot(&self, framebuffer: FramebufferGeometry) -> Option<Vec<u8>> {
        self.display.snapshot_viewport(framebuffer)
    }

    fn restore(&mut self, framebuffer: FramebufferGeometry, pixels: Vec<u8>) -> bool {
        self.display.restore_viewport(framebuffer, pixels)
    }

    fn redraw_app(&self, owner: &ViewportOwner) {
        if kl_kindle::device::usb_state() == Usb::Drive {
            return;
        }
        if let Some(process) =
            system_layer::foreground_process(self.run).filter(|p| p.matches_viewport_owner(owner))
        {
            system_layer::ask_redraw(self.run, process);
        }
    }
    fn grab_touch(&mut self, on: bool) -> bool {
        kl_kindle::input::grab_touch(on)
    }

    fn paint(&mut self, ready: &ReadyViewport, region: Rect, pixels: Vec<u8>) -> bool {
        self.display.present_view_region_matching(
            pixels,
            ready.viewport.logical_size,
            ready.viewport.turn,
            region,
            ready.framebuffer,
        )
    }

    fn paint_sleep(
        &mut self,
        framebuffer: FramebufferGeometry,
        turn: LogicalTurn,
        pixels: Vec<u8>,
    ) -> bool {
        let (width, height) = framebuffer.native_size;
        self.display.set_paused(false);
        let drawn = self.display.present_view_region_matching(
            pixels,
            (width, height),
            turn,
            Rect::new(0, 0, width as i32, height as i32),
            framebuffer,
        );
        self.display.set_paused(true);
        drawn
    }

    fn recorded_generation(&self, owner: &ViewportOwner) -> Option<u64> {
        let live = live_viewport_owner(self.run, Path::new("/proc"))?;
        if &live.owner != owner {
            return None;
        }
        let record = ViewportRecord::read(&owner.viewport_path(self.run))?;
        let after = live_viewport_owner(self.run, Path::new("/proc"))?;
        (&after.owner == owner && record.owner() == owner).then(|| record.viewport().generation)
    }

    fn minimum(&self, owner: &ViewportOwner) -> u64 {
        let path = owner
            .viewport_path(self.run)
            .with_file_name("launcher-occluded");
        read_minimum(&path)
    }

    fn set_minimum(&mut self, owner: &ViewportOwner, generation: Option<u64>) -> bool {
        if live_viewport_owner(self.run, Path::new("/proc"))
            .as_ref()
            .map(|live| &live.owner)
            != Some(owner)
        {
            return false;
        }
        let path = owner
            .viewport_path(self.run)
            .with_file_name("launcher-occluded");
        match generation {
            Some(value) => {
                crate::write_atomic(&path, &format!("{}\n", value.max(read_minimum(&path)))).is_ok()
            }
            None => match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(error) => error.kind() == io::ErrorKind::NotFound,
            },
        }
    }
}

pub fn resume_page_keys(run: &Path) -> io::Result<()> {
    if let Offer::Granted(owner) = offer(run) {
        resume_page_keys_for_owner(run, &owner)?;
    }
    Ok(())
}

fn resume_page_keys_for_owner(run: &Path, owner: &ViewportOwner) -> io::Result<()> {
    if live_viewport_owner(run, Path::new("/proc")).is_some_and(|live| &live.owner == owner) {
        PageKeyFence::new(&owner.viewport_path(run)).resume()?;
    }
    Ok(())
}

fn read_minimum(path: &Path) -> u64 {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return 0,
        Err(_) => return u64::MAX,
    };
    if !metadata.is_file() || metadata.len() > 21 {
        return u64::MAX;
    }
    let mut bytes = String::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()
        .and_then(|file| file.take(22).read_to_string(&mut bytes).ok())
        .and_then(|_| bytes.strip_suffix('\n')?.parse().ok())
        .unwrap_or(u64::MAX)
}

fn checked_paint(
    panel: &mut impl Panel,
    ready: &ReadyViewport,
    region: Rect,
    pixels: Vec<u8>,
) -> bool {
    let generation = ready.viewport.generation;
    panel.ready(generation, true).as_ref() == Some(ready)
        && panel.paint(ready, region, pixels)
        && panel.ready(generation, true).as_ref() == Some(ready)
}

fn paint_sleep_frame(
    panel: &mut impl Panel,
    owner: &ViewportOwner,
    turn: LogicalTurn,
    pixels: Vec<u8>,
) -> bool {
    let Some(framebuffer) = panel.framebuffer() else {
        return false;
    };
    if !panel.freeze(owner, true) {
        return false;
    }
    let drawn = panel.owner_stopped(owner)
        && panel.framebuffer() == Some(framebuffer)
        && panel.paint_sleep(framebuffer, turn, pixels)
        && panel.framebuffer() == Some(framebuffer)
        && panel.owner_stopped(owner);
    panel.freeze(owner, false);
    drawn
}

/// Every normal Launcher draw can use this projected target while the same
/// app is stopped. An invalid target never falls back to a portrait write.
pub struct Output {
    run: PathBuf,
    ready: ReadyViewport,
    logical_pixels: Option<Vec<u8>>,
    failed: bool,
}

impl Output {
    pub fn new(run: &Path, ready: ReadyViewport, logical_pixels: Option<Vec<u8>>) -> Self {
        Self {
            run: run.to_owned(),
            ready,
            logical_pixels,
            failed: false,
        }
    }

    pub fn screen(&self) -> Rect {
        let (w, h) = self.ready.viewport.logical_size;
        Rect::new(0, 0, w as i32, h as i32)
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn verify(&mut self, display: &mut Display) -> bool {
        let mut panel = DevicePanel {
            run: &self.run,
            display,
        };
        if !self.failed
            && panel.ready(self.ready.viewport.generation, true).as_ref() == Some(&self.ready)
        {
            return true;
        }
        abort_output(&mut self.failed, &self.ready, &mut panel);
        false
    }

    pub fn show(&mut self, display: &mut Display, region: Rect, pixels: Vec<u8>, full: bool) {
        if self.failed {
            return;
        }
        let screen = self.screen();
        let area = if let Some(previous) = self.logical_pixels.as_ref().filter(|_| !full) {
            let changed = kl_render::changed_rect(previous, &pixels, screen.w as usize)
                .map(|(x, y, w, h)| Rect::new(x as i32, y as i32, w as i32, h as i32));
            let Some(area) = changed.and_then(|rect| intersection(region, rect)) else {
                self.verify(display);
                return;
            };
            area
        } else {
            region
        };
        let mut panel = DevicePanel {
            run: &self.run,
            display,
        };
        if !checked_paint(&mut panel, &self.ready, area, pixels.clone()) {
            abort_output(&mut self.failed, &self.ready, &mut panel);
        } else if region == screen {
            self.logical_pixels = Some(pixels);
        } else if let Some(previous) = &mut self.logical_pixels {
            let bounds = intersection(screen, area).expect("painted area intersects the screen");
            let width = screen.w as usize;
            for y in bounds.y..bounds.bottom() {
                let row = y as usize * width;
                let columns = row + bounds.x as usize..row + bounds.right() as usize;
                previous[columns.clone()].copy_from_slice(&pixels[columns]);
            }
        }
    }
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let w = a.right().min(b.right()) - x;
    let h = a.bottom().min(b.bottom()) - y;
    (w > 0 && h > 0).then(|| Rect::new(x, y, w, h))
}

fn abort_output(failed: &mut bool, ready: &ReadyViewport, panel: &mut impl Panel) {
    if !std::mem::replace(failed, true) {
        panel.grab_touch(false);
        panel.redraw_app(&ready.owner);
        panel.freeze(&ready.owner, false);
        panel.resume_page_keys(&ready.owner);
    }
}

struct Captured {
    ready: ReadyViewport,
    pixels: Vec<u8>,
    dark: bool,
}

impl Captured {
    fn freeze(panel: &mut impl Panel, ready: &ReadyViewport, dark: bool) -> Option<Self> {
        let generation = ready.viewport.generation;
        if !panel.freeze(&ready.owner, true) {
            return None;
        }
        let capture = (|| {
            if panel.ready(generation, true).as_ref() != Some(ready) {
                return None;
            }
            let pixels = panel.snapshot(ready.framebuffer)?;
            if panel.ready(generation, true).as_ref() != Some(ready) {
                return None;
            }
            // Survive a Launcher restart between the first write and restore.
            if !panel.set_minimum(&ready.owner, Some(generation.checked_add(1)?)) {
                return None;
            }
            if !panel.hold_page_keys(&ready.owner) {
                panel.set_minimum(&ready.owner, None);
                return None;
            }
            Some(Self {
                ready: ready.clone(),
                pixels,
                dark,
            })
        })();
        if capture.is_none() {
            panel.freeze(&ready.owner, false);
        }
        capture
    }

    fn backdrop(&self, dark: bool) -> Option<Vec<u8>> {
        let ready = &self.ready;
        let mut pixels = ViewportTransform::new(
            ready.framebuffer.native_size,
            ready.viewport.turn,
            ready.framebuffer.turn,
        )?
        .unproject(self.pixels.clone())?;
        if self.dark != dark {
            system_layer::in_other_theme(&mut pixels);
        }
        Some(pixels)
    }

    fn release(self, panel: &mut impl Panel, restore: bool, redraw: bool, dark: bool) -> bool {
        let ready = &self.ready;
        let restored = restore
            && self.dark == dark
            && panel.ready(ready.viewport.generation, true).as_ref() == Some(ready)
            && panel.restore(ready.framebuffer, self.pixels)
            && panel.ready(ready.viewport.generation, true).as_ref() == Some(ready)
            && panel.set_minimum(&ready.owner, None);
        if redraw && !restored {
            panel.redraw_app(&ready.owner);
        }
        panel.grab_touch(false);
        panel.freeze(&ready.owner, false);
        panel.resume_page_keys(&ready.owner);
        restored
    }
}

pub enum Reaction {
    Legacy(Input),
    Consumed,
    Opened(Box<Renderer>),
    Projected(Input),
}

/// The layout/render caches for Launcher Home are retained during the app's overlay.
pub struct Surface {
    home: Renderer,
    layout: Layout,
    rotation: Rotation,
    auto_rotation: bool,
}

impl Surface {
    pub fn open(
        ready: &ReadyViewport,
        candidate: Renderer,
        renderer: &mut Renderer,
        ui: &mut Ui,
        settings: &Settings,
    ) -> Self {
        let (width, height) = ready.viewport.logical_size;
        let layout = ui.layout;
        ui.layout = Layout::new(width, height, layout.cols as u32, layout.rows as u32);
        ui.screen = Screen::QuickSettings;
        ui.pressed = None;
        Self {
            home: std::mem::replace(renderer, candidate),
            layout,
            rotation: settings.rotation,
            auto_rotation: settings.auto_rotation,
        }
    }

    pub fn same_rotation(&self, settings: &Settings) -> bool {
        self.rotation == settings.rotation && self.auto_rotation == settings.auto_rotation
    }

    pub fn home_renderer(&mut self) -> &mut Renderer {
        &mut self.home
    }

    pub fn home_layout(&self, ui: &Ui) -> Layout {
        Layout::new(
            self.layout.width as u32,
            self.layout.height as u32,
            ui.layout.cols as u32,
            ui.layout.rows as u32,
        )
    }

    pub fn close(
        surface: &mut Option<Self>,
        display: &mut system_layer::OverlayDisplay,
        renderer: &mut Renderer,
        ui: &mut Ui,
    ) {
        if let Some(surface) = surface.take() {
            let layout = surface.home_layout(ui);
            *renderer = surface.home;
            ui.layout = layout;
            ui.finish_wallpaper_preview();
            ui.screen = Screen::Home;
            ui.pressed = None;
            display.clear_projection();
        }
    }
}

pub struct Layer {
    pub generation: Arc<ViewportGeneration>,
    latest: Option<ReadyViewport>,
    current: Option<ReadyViewport>,
    captured: Option<Captured>,
    asleep: bool,
    wake_seen: bool,
    usb: Usb,
    required_generation: u64,
    bound_owner: Option<ViewportOwner>,
    scoped: bool,
}

impl Layer {
    pub fn new(usb: Usb) -> Self {
        Self {
            generation: Arc::new(ViewportGeneration::default()),
            latest: None,
            current: None,
            captured: None,
            asleep: false,
            wake_seen: false,
            usb,
            required_generation: 0,
            bound_owner: None,
            scoped: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.captured.is_some()
    }

    pub fn ready(&self) -> Option<&ReadyViewport> {
        self.captured.as_ref().map(|capture| &capture.ready)
    }

    pub fn backdrop(&self, dark: bool) -> Option<Vec<u8>> {
        self.captured.as_ref()?.backdrop(dark)
    }

    /// Stock's waiting dialog continues in the saved Launcher rotation.
    pub fn stock_frame(&self, pixels: Vec<u8>, rotation: Rotation) -> Option<Vec<u8>> {
        let ready = self.ready()?;
        let FramebufferGeometry {
            native_size, turn, ..
        } = ready.framebuffer;
        let packed = ViewportTransform::new(native_size, rotation.into(), turn)?.project(pixels)?;
        ViewportTransform::new(native_size, ready.viewport.turn, turn)?.unproject(packed)
    }

    pub fn scoped_sleep(&self) -> bool {
        self.scoped && self.asleep && self.usb != Usb::Drive
    }

    pub fn show_sleep(&self, panel: &mut DevicePanel<'_>, rotation: Rotation, pixels: Vec<u8>) {
        if self.scoped_sleep() {
            if let Offer::Granted(owner) = offer(panel.run) {
                if self.bound_owner.as_ref() == Some(&owner) {
                    paint_sleep_frame(panel, &owner, rotation.into(), pixels);
                }
            }
        }
    }

    fn select(&mut self, candidate: Option<ReadyViewport>) {
        if self.current != candidate {
            self.current = candidate;
            self.generation.advance();
        }
    }

    fn bind_owner(&mut self, owner: &ViewportOwner, panel: &mut impl Panel, dark: bool) {
        if self
            .bound_owner
            .as_ref()
            .is_some_and(|previous| previous != owner)
        {
            self.close(panel, false, false, dark);
            self.reset_owner();
        }
        self.bound_owner = Some(owner.clone());
        self.scoped = true;
    }

    fn observe(&mut self, owner: &ViewportOwner, panel: &mut impl Panel, dark: bool) {
        self.bind_owner(owner, panel, dark);
        if self.asleep || self.usb == Usb::Drive {
            self.select(None);
            return;
        }
        if let Some(capture) = self.ready() {
            let still_captured =
                panel.ready(capture.viewport.generation, true).as_ref() == Some(capture);
            if !still_captured {
                self.close(panel, false, true, dark);
            }
            return;
        }
        let floor = panel.minimum(owner).max(self.required_generation);
        let minimum = floor.max(self.latest.as_ref().map_or(0, |r| r.viewport.generation));
        let observed = panel
            .ready(minimum, false)
            .filter(|ready| &ready.owner == owner);
        if let Some(ready) = &observed {
            self.latest = Some(ready.clone());
            if floor != 0 && panel.set_minimum(owner, None) {
                self.required_generation = 0;
            }
        }
        self.select(observed);
    }

    pub fn finish(&mut self, panel: &mut DevicePanel<'_>, restore: bool, redraw: bool, dark: bool) {
        self.close(panel, restore, redraw, dark);
    }

    fn close(&mut self, panel: &mut impl Panel, restore: bool, redraw: bool, dark: bool) {
        if let Some(capture) = self.captured.take() {
            if !capture.release(panel, restore, redraw, dark) {
                self.select(None);
            }
            self.generation.advance();
        }
    }

    /// Output already released touch and the original owner after an invalid write.
    pub fn abandon(&mut self) {
        if self.captured.take().is_some() {
            self.select(None);
            self.generation.advance();
        }
    }

    fn reset_owner(&mut self) {
        self.bound_owner = None;
        self.scoped = false;
        self.latest = None;
        self.current = None;
        self.required_generation = 0;
        self.generation.advance();
    }

    pub fn shared(&mut self, event: &Ev, panel: &mut DevicePanel<'_>, dark: bool) -> bool {
        self.shared_inner(event, offer(panel.run), panel, dark)
    }

    fn shared_inner(
        &mut self,
        event: &Ev,
        offer: Offer,
        panel: &mut impl Panel,
        dark: bool,
    ) -> bool {
        self.scoped |= offer != Offer::Legacy;
        let scoped = self.scoped;
        if let Offer::Granted(owner) = &offer {
            self.bind_owner(owner, panel, dark);
        } else if scoped {
            self.select(None);
            let redraw = !matches!(
                event,
                Ev::AppEnded | Ev::Usb(Usb::Drive) | Ev::Input(Input::Sleep)
            );
            self.close(panel, false, redraw, dark);
        }
        match event {
            Ev::AppEnded => {
                self.close(panel, false, false, dark);
                self.reset_owner();
            }
            Ev::Usb(usb) if *usb != self.usb => {
                let was_drive = self.usb == Usb::Drive;
                self.usb = *usb;
                if scoped && (*usb == Usb::Drive || was_drive) {
                    self.stock_wrote(panel, dark);
                    if let Offer::Granted(owner) = &offer {
                        self.resume_after_stock(panel, owner);
                    }
                }
            }
            Ev::Input(Input::Sleep) if !self.asleep => {
                self.asleep = true;
                self.wake_seen = false;
                if scoped {
                    self.stock_wrote(panel, dark);
                }
            }
            Ev::Input(Input::Wake) if !self.wake_seen => {
                self.asleep = false;
                self.wake_seen = true;
                if let Offer::Granted(owner) = &offer {
                    self.stock_wrote(panel, dark);
                    self.resume_after_stock(panel, owner);
                }
            }
            Ev::Tick => {
                if let Offer::Granted(owner) = &offer {
                    self.observe(owner, panel, dark);
                }
            }
            _ => {}
        }
        scoped && matches!(event, Ev::Input(Input::Sleep | Input::Wake))
    }

    fn stock_wrote(&mut self, panel: &mut impl Panel, dark: bool) {
        if let Some(owner) = &self.bound_owner {
            let recorded = panel.recorded_generation(owner).unwrap_or(0);
            let latest = self.latest.as_ref().map_or(0, |r| r.viewport.generation);
            let next = recorded.max(latest).saturating_add(1);
            self.required_generation = self.required_generation.max(next);
            panel.set_minimum(owner, Some(self.required_generation));
        }
        self.select(None);
        self.close(panel, false, false, dark);
    }

    fn resume_after_stock(&self, panel: &impl Panel, owner: &ViewportOwner) {
        if !self.asleep && self.usb != Usb::Drive {
            panel.redraw_app(owner);
        }
    }

    pub fn native(
        &mut self,
        event: ViewportEvent,
        panel: &mut DevicePanel<'_>,
        dark: bool,
        fonts: impl FnOnce() -> Vec<String>,
    ) -> Reaction {
        self.native_inner(event, offer(panel.run), panel, dark, fonts)
    }

    fn native_inner(
        &mut self,
        event: ViewportEvent,
        offer: Offer,
        panel: &mut impl Panel,
        dark: bool,
        fonts: impl FnOnce() -> Vec<String>,
    ) -> Reaction {
        self.scoped |= offer != Offer::Legacy;
        let Offer::Granted(owner) = offer else {
            if self.scoped {
                self.select(None);
                self.close(panel, false, true, dark);
                return Reaction::Consumed;
            }
            return event
                .resolve(&self.generation, LogicalTurn::Upright)
                .map_or(Reaction::Consumed, |input| Reaction::Legacy(input.input));
        };
        self.observe(&owner, panel, dark);
        let Some(ready) = self.current.clone() else {
            return Reaction::Consumed;
        };
        let Some(resolved) = event.resolve(&self.generation, ready.viewport.turn) else {
            return Reaction::Consumed;
        };
        if self.is_open() {
            return Reaction::Projected(resolved.input);
        }
        let (w, h) = ready.viewport.logical_size;
        let layout = Layout::new(w, h, 4, 5);
        if !system_layer::opens_quick_settings(resolved.input, layout.status().bottom()) {
            return Reaction::Consumed;
        }
        let Ok(renderer) = Renderer::new(w, h, &fonts()) else {
            return Reaction::Consumed;
        };
        let Some(captured) = Captured::freeze(panel, &ready, dark) else {
            return Reaction::Consumed;
        };
        if !panel.grab_touch(true) {
            if !captured.release(panel, true, true, dark) {
                self.select(None);
            }
            return Reaction::Consumed;
        }
        self.captured = Some(captured);
        self.generation.advance();
        Reaction::Opened(Box::new(renderer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kl_core::{
        layout::{Page, QuickTile},
        Action, DeviceCmd, Status,
    };
    use kl_kindle::{
        input::{NativeInput, NativeTouch, TouchPhase},
        profile,
    };
    use std::{cell::Cell, time::Duration};

    fn ready(geometry: LogicalTurn, framebuffer_turn: u32, generation: u64) -> ReadyViewport {
        let native_size = (profile::ACTIVE.screen_w, profile::ACTIVE.screen_h);
        let framebuffer = FramebufferGeometry {
            native_size,
            turn: framebuffer_turn,
            yoffset: 13,
            pitch: native_size.0 + 16,
        };
        ReadyViewport {
            owner: ViewportOwner::parse("v1 43 8701 app-turn.ABC123").unwrap(),
            viewport: kl_kindle::viewport::Viewport {
                generation,
                turn: geometry,
                logical_size: ViewportTransform::new(native_size, geometry, framebuffer_turn)
                    .unwrap()
                    .logical_size(),
            },
            framebuffer,
        }
    }

    struct Mock {
        record: Option<ReadyViewport>,
        after_freeze: Option<ReadyViewport>,
        after_capture: Option<ReadyViewport>,
        after_paint: Option<ReadyViewport>,
        after_restore: Option<ReadyViewport>,
        ready_valid: bool,
        storage_exported: bool,
        stopped: bool,
        grabbed: bool,
        original: Vec<u8>,
        pixels: Vec<u8>,
        snapshot_reads: Cell<usize>,
        restored: usize,
        shows: usize,
        sleep_shows: usize,
        keys_held: Cell<bool>,
        key_resumes: Cell<usize>,
        redraws: Cell<usize>,
        redraws_while_stopped: Cell<usize>,
        minimum: Option<(ViewportOwner, u64)>,
    }

    impl Mock {
        fn new(ready: ReadyViewport) -> Self {
            let (width, height) = ready.framebuffer.native_size;
            let original: Vec<_> = (0..width * height).map(|i| (i % 223) as u8).collect();
            Self {
                record: Some(ready),
                after_freeze: None,
                after_capture: None,
                after_paint: None,
                after_restore: None,
                ready_valid: true,
                storage_exported: false,
                stopped: false,
                grabbed: false,
                pixels: original.clone(),
                original,
                snapshot_reads: Cell::new(0),
                restored: 0,
                shows: 0,
                sleep_shows: 0,
                keys_held: Cell::new(false),
                key_resumes: Cell::new(0),
                redraws: Cell::new(0),
                redraws_while_stopped: Cell::new(0),
                minimum: None,
            }
        }
    }

    impl Panel for Mock {
        fn framebuffer(&self) -> Option<FramebufferGeometry> {
            self.record
                .as_ref()
                .filter(|_| !self.storage_exported)
                .map(|ready| ready.framebuffer)
        }

        fn owner_stopped(&self, owner: &ViewportOwner) -> bool {
            self.stopped
                && self
                    .record
                    .as_ref()
                    .is_some_and(|ready| &ready.owner == owner)
        }

        fn hold_page_keys(&self, owner: &ViewportOwner) -> bool {
            assert!(self.owner_stopped(owner));
            self.keys_held.set(true);
            true
        }

        fn resume_page_keys(&self, owner: &ViewportOwner) {
            if self
                .record
                .as_ref()
                .is_some_and(|ready| &ready.owner == owner)
            {
                assert!(!self.stopped);
                assert!(self.keys_held.replace(false));
                self.key_resumes.set(self.key_resumes.get() + 1);
            }
        }

        fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport> {
            if !self.ready_valid || self.storage_exported || (stopped && !self.stopped) {
                return None;
            }
            let record = if self.snapshot_reads.get() > 0 && self.after_capture.is_some() {
                self.after_capture.as_ref()
            } else {
                self.record.as_ref()
            };
            record
                .filter(|ready| ready.viewport.generation >= minimum)
                .cloned()
        }

        fn freeze(&mut self, owner: &ViewportOwner, on: bool) -> bool {
            if !self.record.as_ref().is_some_and(|r| &r.owner == owner) {
                return false;
            }
            self.stopped = on;
            if on {
                if let Some(changed) = self.after_freeze.take() {
                    self.record = Some(changed);
                }
            }
            true
        }

        fn snapshot(&self, geometry: FramebufferGeometry) -> Option<Vec<u8>> {
            let record = self.record.as_ref()?;
            if !self.stopped || record.framebuffer != geometry {
                return None;
            }
            self.snapshot_reads.set(self.snapshot_reads.get() + 1);
            Some(self.pixels.clone())
        }

        fn restore(&mut self, geometry: FramebufferGeometry, pixels: Vec<u8>) -> bool {
            if !self.stopped
                || self
                    .record
                    .as_ref()
                    .is_none_or(|r| r.framebuffer != geometry)
            {
                return false;
            }
            self.restored += 1;
            self.pixels = pixels;
            if let Some(changed) = self.after_restore.take() {
                self.record = Some(changed);
            }
            true
        }

        fn redraw_app(&self, owner: &ViewportOwner) {
            if self.storage_exported {
                return;
            }
            if self
                .record
                .as_ref()
                .is_some_and(|record| &record.owner == owner)
            {
                self.redraws.set(self.redraws.get() + 1);
                if self.stopped {
                    self.redraws_while_stopped
                        .set(self.redraws_while_stopped.get() + 1);
                }
            }
        }

        fn grab_touch(&mut self, on: bool) -> bool {
            self.grabbed = on;
            true
        }

        fn paint(&mut self, ready: &ReadyViewport, region: Rect, pixels: Vec<u8>) -> bool {
            if !self.stopped || !self.grabbed || self.record.as_ref() != Some(ready) {
                return false;
            }
            let transform = transform(ready);
            let mut logical = transform.unproject(self.pixels.clone()).unwrap();
            let width = ready.viewport.logical_size.0 as usize;
            for y in region.y..region.bottom() {
                let left = y as usize * width + region.x as usize;
                let right = left + region.w as usize;
                logical[left..right].copy_from_slice(&pixels[left..right]);
            }
            self.pixels = transform.project(logical).unwrap();
            self.shows += 1;
            if let Some(changed) = self.after_paint.take() {
                self.record = Some(changed);
            }
            true
        }

        fn paint_sleep(
            &mut self,
            geometry: FramebufferGeometry,
            turn: LogicalTurn,
            pixels: Vec<u8>,
        ) -> bool {
            assert!(self.stopped && !self.grabbed);
            self.sleep_shows += 1;
            self.pixels = ViewportTransform::new(geometry.native_size, turn, geometry.turn)
                .unwrap()
                .project(pixels)
                .unwrap();
            if let Some(changed) = self.after_paint.take() {
                self.record = Some(changed);
            }
            true
        }

        fn recorded_generation(&self, owner: &ViewportOwner) -> Option<u64> {
            self.record
                .as_ref()
                .filter(|record| &record.owner == owner)
                .map(|r| r.viewport.generation)
        }

        fn minimum(&self, owner: &ViewportOwner) -> u64 {
            self.minimum
                .as_ref()
                .filter(|(previous, _)| previous == owner)
                .map_or(0, |(_, value)| *value)
        }

        fn set_minimum(&mut self, owner: &ViewportOwner, generation: Option<u64>) -> bool {
            if self.recorded_generation(owner).is_none() {
                return false;
            }
            self.minimum = generation.map(|value| (owner.clone(), value.max(self.minimum(owner))));
            true
        }
    }

    fn transform(ready: &ReadyViewport) -> ViewportTransform {
        ViewportTransform::new(
            ready.framebuffer.native_size,
            ready.viewport.turn,
            ready.framebuffer.turn,
        )
        .unwrap()
    }

    fn native_point(ready: &ReadyViewport, (x, y): (f32, f32)) -> (f32, f32) {
        let (width, height) = ready.framebuffer.native_size;
        match ready.viewport.turn {
            LogicalTurn::Upright => (x, y),
            LogicalTurn::Clockwise => (y, height as f32 - 1.0 - x),
            LogicalTurn::UpsideDown => (width as f32 - 1.0 - x, height as f32 - 1.0 - y),
            LogicalTurn::Counterclockwise => (width as f32 - 1.0 - y, x),
        }
    }

    fn gesture(
        layer: &Layer,
        ready: &ReadyViewport,
        from: (f32, f32),
        to: (f32, f32),
    ) -> ViewportEvent {
        ViewportEvent {
            generation: layer.generation.current(),
            native: NativeInput::Touch(NativeTouch {
                start: native_point(ready, from),
                end: native_point(ready, to),
                phase: TouchPhase::Finished(Duration::from_millis(100)),
            }),
        }
    }

    fn fonts() -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"]
            .into_iter()
            .map(|name| root.join(name).to_string_lossy().into_owned())
            .collect()
    }

    fn feed(
        layer: &mut Layer,
        panel: &mut Mock,
        ready: &ReadyViewport,
        status: &Status,
        event: ViewportEvent,
    ) -> Reaction {
        layer.native_inner(
            event,
            Offer::Granted(ready.owner.clone()),
            panel,
            status.dark_mode,
            fonts,
        )
    }

    fn status() -> Status {
        let mut status = Status {
            clock: "9:41".into(),
            light: 9,
            warmth: 5,
            ..Status::default()
        };
        status.display_support.front_light = true;
        status.display_support.warmth = true;
        status
    }

    fn center(rect: Rect) -> (f32, f32) {
        ((rect.x + rect.w / 2) as f32, (rect.y + rect.h / 2) as f32)
    }

    struct SystemUi {
        ui: Ui,
        renderer: Renderer,
        home: Option<Surface>,
        display: system_layer::OverlayDisplay,
    }

    impl SystemUi {
        fn open(
            layer: &mut Layer,
            panel: &mut Mock,
            ready: &ReadyViewport,
            status: &Status,
        ) -> Self {
            layer.observe(&ready.owner, panel, status.dark_mode);
            let swipe = gesture(layer, ready, (200.0, 20.0), (200.0, 370.0));
            let Reaction::Opened(candidate) = feed(layer, panel, ready, status, swipe) else {
                panic!("eligible top swipe did not open Quick Settings");
            };
            let (w, h) = ready.framebuffer.native_size;
            let mut ui = Ui::new(Vec::new(), Layout::new(w, h, 4, 5));
            ui.display_support = status.display_support;
            let mut renderer = Renderer::new(w, h, &fonts()).unwrap();
            let home = Surface::open(
                ready,
                *candidate,
                &mut renderer,
                &mut ui,
                &Settings::default(),
            );
            renderer.set_backdrop(layer.backdrop(status.dark_mode));
            let pixels = renderer.draw(&ui, status);
            assert!(checked_paint(panel, ready, ui.layout.quick_panel(), pixels));
            Self {
                ui,
                renderer,
                home: Some(home),
                display: system_layer::OverlayDisplay::open(),
            }
        }

        fn apply(
            &mut self,
            reaction: Reaction,
            layer: &mut Layer,
            panel: &mut Mock,
            status: &Status,
        ) -> Action {
            let Reaction::Projected(input) = reaction else {
                return Action::None;
            };
            let action = self.ui.handle(input);
            if self.ui.screen == Screen::Home {
                layer.close(panel, true, true, status.dark_mode);
                Surface::close(
                    &mut self.home,
                    &mut self.display,
                    &mut self.renderer,
                    &mut self.ui,
                );
            } else if matches!(action, Action::Redraw { .. }) {
                let pixels = self.renderer.draw(&self.ui, status);
                assert!(checked_paint(
                    panel,
                    layer.ready().unwrap(),
                    self.ui.layout.screen(),
                    pixels
                ));
            }
            action
        }

        fn tap(
            &mut self,
            layer: &mut Layer,
            panel: &mut Mock,
            ready: &ReadyViewport,
            status: &Status,
            rect: Rect,
        ) -> Action {
            let position = center(rect);
            let event = gesture(layer, ready, position, position);
            let reaction = feed(layer, panel, ready, status, event);
            self.apply(reaction, layer, panel, status)
        }
    }

    #[test]
    fn actual_quick_settings_paints_and_accepts_controls_in_the_foreground_viewport() {
        for turn in [
            LogicalTurn::Upright,
            LogicalTurn::Clockwise,
            LogicalTurn::UpsideDown,
            LogicalTurn::Counterclockwise,
        ] {
            for firmware in [0, 2] {
                let ready = ready(turn, firmware, 4);
                let mut panel = Mock::new(ready.clone());
                let mut layer = Layer::new(Usb::Unplugged);
                let status = status();
                layer.observe(&ready.owner, &mut panel, false);
                let (w, h) = ready.viewport.logical_size;
                let x = w as f32 / 2.0;
                let bottom = h as f32 - 20.0;
                let swipe_bottom = gesture(&layer, &ready, (x, bottom), (x, bottom - 350.0));
                feed(&mut layer, &mut panel, &ready, &status, swipe_bottom);
                assert!(!layer.is_open());

                let mut surface = SystemUi::open(&mut layer, &mut panel, &ready, &status);
                assert!(layer.is_open() && panel.stopped && panel.grabbed);
                assert_ne!(panel.pixels, panel.original);
                assert_eq!(panel.shows, 1);
                let project = transform(&ready);
                let cutoff = surface.ui.layout.quick_panel().bottom() as usize * w as usize;
                assert_eq!(
                    project.unproject(panel.pixels.clone()).unwrap()[cutoff..],
                    project.unproject(panel.original.clone()).unwrap()[cutoff..],
                );

                let plus = surface.ui.layout.quick_plus(0);
                assert_eq!(
                    surface.tap(&mut layer, &mut panel, &ready, &status, plus),
                    Action::Device(DeviceCmd::Light(1))
                );
                let swaps = matches!(turn, LogicalTurn::Clockwise | LogicalTurn::UpsideDown);
                let mut keys = [
                    profile::ACTIVE.page_forward_key,
                    profile::ACTIVE.page_back_key,
                ];
                if swaps {
                    keys.reverse();
                }
                for (i, key) in keys.into_iter().enumerate() {
                    let event = ViewportEvent {
                        generation: layer.generation.current(),
                        native: NativeInput::PageKey(key),
                    };
                    let reaction = feed(&mut layer, &mut panel, &ready, &status, event);
                    surface.apply(reaction, &mut layer, &mut panel, &status);
                    assert_eq!(layer.is_open(), i == 0);
                }
                assert!(!panel.stopped && !panel.grabbed && !panel.keys_held.get());
                assert_eq!(panel.key_resumes.get(), 1);
                assert_eq!(panel.restored, 1);
                assert_eq!(panel.minimum(&ready.owner), 0);
                assert_eq!(panel.pixels, panel.original);
                assert_eq!(
                    surface.ui.layout.width as u32,
                    ready.framebuffer.native_size.0
                );
            }
        }
    }

    #[test]
    fn settings_battery_and_power_use_the_same_logical_frame_and_restore() {
        for (turn, firmware) in [
            (LogicalTurn::Clockwise, 2),
            (LogicalTurn::Counterclockwise, 0),
        ] {
            let ready = ready(turn, firmware, 7);
            let mut layer = Layer::new(Usb::Unplugged);
            let mut panel = Mock::new(ready.clone());
            let status = status();
            let mut surface = SystemUi::open(&mut layer, &mut panel, &ready, &status);
            let layout = surface.ui.layout;

            let settings = layout
                .quick_buttons()
                .into_iter()
                .find(|(tile, _)| *tile == QuickTile::AllSettings)
                .unwrap()
                .1;
            surface.tap(&mut layer, &mut panel, &ready, &status, settings);
            assert_eq!(surface.ui.screen, Screen::Settings);
            let battery = layout
                .settings_home_rows(0)
                .into_iter()
                .find(|(page, _)| *page == Page::Battery)
                .unwrap()
                .1;
            surface.tap(&mut layer, &mut panel, &ready, &status, battery);
            assert_eq!(surface.ui.screen, Screen::Page(Page::Battery));
            for expected in [Screen::Settings, Screen::Home] {
                surface.apply(
                    Reaction::Projected(Input::PageBack),
                    &mut layer,
                    &mut panel,
                    &status,
                );
                assert_eq!(surface.ui.screen, expected);
            }
            assert_eq!(panel.restored, 1);
            assert_eq!(panel.pixels, panel.original);

            let mut surface = SystemUi::open(&mut layer, &mut panel, &ready, &status);
            let power = layout
                .quick_buttons()
                .into_iter()
                .find(|(tile, _)| *tile == QuickTile::Power)
                .unwrap()
                .1;
            let [(_, cancel), (_, kindle), (_, restart), (_, off)] = layout.power_buttons();
            surface.tap(&mut layer, &mut panel, &ready, &status, power);
            assert_eq!(surface.ui.screen, Screen::PowerMenu);
            assert_eq!(
                surface.tap(&mut layer, &mut panel, &ready, &status, restart),
                Action::Device(DeviceCmd::Restart)
            );
            assert_eq!(
                surface.tap(&mut layer, &mut panel, &ready, &status, off),
                Action::Device(DeviceCmd::PowerOff)
            );
            surface.tap(&mut layer, &mut panel, &ready, &status, kindle);
            assert_eq!(surface.ui.screen, Screen::ConfirmKindle);
            let [(_, keep_reading), (_, open)] = layout.confirm_buttons();
            assert_eq!(
                surface.tap(&mut layer, &mut panel, &ready, &status, open),
                Action::KindleMode
            );
            surface.tap(&mut layer, &mut panel, &ready, &status, keep_reading);
            assert_eq!(surface.ui.screen, Screen::Home);
            assert_eq!(panel.restored, 2);

            let mut surface = SystemUi::open(&mut layer, &mut panel, &ready, &status);
            surface.tap(&mut layer, &mut panel, &ready, &status, power);
            surface.tap(&mut layer, &mut panel, &ready, &status, cancel);
            assert_eq!(surface.ui.screen, Screen::Home);
            assert_eq!(panel.restored, 3);
            assert_eq!(panel.pixels, panel.original);
        }
    }

    #[test]
    fn capture_and_restore_refuse_generation_geometry_and_owner_replacements() {
        let base = ready(LogicalTurn::Clockwise, 0, 4);
        let resized = ready(LogicalTurn::Counterclockwise, 0, 5);
        let mut panel = Mock::new(base.clone());
        panel.after_freeze = Some(resized.clone());
        assert!(Captured::freeze(&mut panel, &base, false).is_none());
        assert!(!panel.stopped);
        assert_eq!(panel.snapshot_reads.get(), 0);

        panel.record = Some(base.clone());
        panel.after_capture = Some(resized);
        assert!(Captured::freeze(&mut panel, &base, false).is_none());
        assert!(!panel.stopped);
        panel.after_capture = None;

        let snapshot = Captured::freeze(&mut panel, &base, false).unwrap();
        let mut changed_geometry = base.clone();
        changed_geometry.framebuffer.yoffset += 1;
        panel.record = Some(changed_geometry);
        snapshot.release(&mut panel, true, true, false);
        assert_eq!(panel.restored, 0);
        assert!(!panel.stopped);

        panel.record = Some(base.clone());
        let snapshot = Captured::freeze(&mut panel, &base, false).unwrap();
        panel.record.as_mut().unwrap().owner =
            ViewportOwner::parse("v1 43 8702 app-turn.DEF456").unwrap();
        snapshot.release(&mut panel, true, true, false);
        assert_eq!(panel.restored, 0);
    }

    #[test]
    fn storage_and_sleep_close_the_panel_and_quarantine_preexisting_ready_records() {
        for (enter, exit) in [
            (Ev::Input(Input::Sleep), Ev::Input(Input::Wake)),
            (Ev::Usb(Usb::Drive), Ev::Usb(Usb::Unplugged)),
        ] {
            let old = ready(LogicalTurn::Clockwise, 0, 4);
            let newer = ready(LogicalTurn::Clockwise, 0, 5);
            let mut panel = Mock::new(old.clone());
            let mut layer = Layer::new(Usb::Unplugged);
            let status = status();
            layer.observe(&old.owner, &mut panel, false);
            let _surface = SystemUi::open(&mut layer, &mut panel, &old, &status);
            assert!(layer.is_open());
            layer.shared_inner(&enter, Offer::Granted(old.owner.clone()), &mut panel, false);
            assert!(!layer.is_open() && !panel.grabbed && !panel.stopped);
            assert_eq!(panel.restored, 0);
            layer.shared_inner(&exit, Offer::Granted(old.owner.clone()), &mut panel, false);
            layer.observe(&old.owner, &mut panel, false);
            assert!(layer.current.is_none());
            let blocked_swipe = gesture(&layer, &old, (200.0, 20.0), (200.0, 370.0));
            feed(&mut layer, &mut panel, &old, &status, blocked_swipe);
            assert_eq!(panel.shows, 1);
            panel.record = Some(newer.clone());
            layer.observe(&newer.owner, &mut panel, false);
            assert_eq!(layer.current, Some(newer));
        }
    }

    #[test]
    fn exposure_and_restore_fail_closed_if_the_bound_frame_changes_mid_write() {
        let old = ready(LogicalTurn::Clockwise, 0, 5);
        let mut shifted = old.clone();
        shifted.framebuffer.pitch += 32;
        let status = status();

        let mut layer = Layer::new(Usb::Unplugged);
        let mut panel = Mock::new(old.clone());
        let _surface = SystemUi::open(&mut layer, &mut panel, &old, &status);
        let screen = Rect::new(
            0,
            0,
            old.viewport.logical_size.0 as i32,
            old.viewport.logical_size.1 as i32,
        );
        let target = vec![124; panel.pixels.len()];
        let tap = gesture(&layer, &old, (275.0, 193.0), (275.0, 193.0));
        panel.after_paint = Some(shifted.clone());
        assert!(!checked_paint(&mut panel, &old, screen, target));
        let mut failed = false;
        abort_output(&mut failed, &old, &mut panel);
        abort_output(&mut failed, &old, &mut panel);
        layer.abandon();
        assert!(failed && !panel.grabbed && !panel.stopped && !panel.keys_held.get());
        assert_eq!(panel.key_resumes.get(), 1);
        assert_eq!(panel.redraws.get(), 1);
        assert_eq!(panel.redraws_while_stopped.get(), 1);
        assert_eq!(panel.restored, 0);
        assert_eq!(panel.minimum(&old.owner), 6);
        assert!(matches!(
            feed(&mut layer, &mut panel, &old, &status, tap),
            Reaction::Consumed
        ));

        let newer = ready(LogicalTurn::Counterclockwise, 2, 6);
        panel.record = Some(newer.clone());
        let _surface = SystemUi::open(&mut layer, &mut panel, &newer, &status);
        shifted = newer.clone();
        shifted.framebuffer.yoffset += 1;
        panel.after_restore = Some(shifted);
        layer.close(&mut panel, true, true, false);
        assert_eq!(panel.restored, 1);
        assert_eq!(panel.minimum(&newer.owner), 7);
        assert_eq!(panel.redraws.get(), 2);
        assert!(!panel.stopped && !panel.grabbed);
    }

    #[test]
    fn theme_change_and_terminal_release_quarantine_instead_of_restoring() {
        let base = ready(LogicalTurn::Counterclockwise, 2, 8);
        let mut panel = Mock::new(base.clone());
        let mut layer = Layer::new(Usb::Unplugged);
        let surface = SystemUi::open(&mut layer, &mut panel, &base, &status());
        let rotated = Settings {
            rotation: Rotation::UpsideDown,
            ..Settings::default()
        };
        assert!(!surface.home.as_ref().unwrap().same_rotation(&rotated));
        layer.close(&mut panel, true, true, true);
        assert_eq!(panel.minimum(&base.owner), 9);
        assert_eq!(panel.restored, 0);
        assert_eq!(panel.redraws.get(), 1);

        let visible = ready(LogicalTurn::Clockwise, 2, 9);
        panel.record = Some(visible.clone());
        let _surface = SystemUi::open(&mut layer, &mut panel, &visible, &status());
        let portrait: Vec<_> = (0..panel.pixels.len()).map(|i| (i % 179) as u8).collect();
        let projected_wait = layer
            .stock_frame(portrait.clone(), rotated.rotation)
            .unwrap();
        let native_stock =
            ViewportTransform::new(visible.framebuffer.native_size, rotated.rotation.into(), 2)
                .unwrap()
                .project(portrait)
                .unwrap();
        assert_eq!(
            transform(&visible).project(projected_wait).unwrap(),
            native_stock
        );
        layer.close(&mut panel, false, false, true);
        assert_eq!(panel.minimum(&visible.owner), 10);
        assert_eq!(panel.restored, 0);
        assert_eq!(panel.redraws.get(), 1);
        assert!(!panel.stopped && !panel.grabbed);

        // A new Launcher process must not capture that terminal or waiting picture as an app page.
        let mut restarted = Layer::new(Usb::Unplugged);
        restarted.observe(&visible.owner, &mut panel, false);
        assert_eq!(restarted.current, None);
        let fresh = ready(LogicalTurn::Clockwise, 2, 10);
        panel.record = Some(fresh.clone());
        restarted.observe(&fresh.owner, &mut panel, false);
        assert_eq!(restarted.current, Some(fresh));
        assert_eq!(panel.minimum(&visible.owner), 0);
    }

    #[test]
    fn overlapping_sleep_and_usb_never_restore_or_redraw_under_the_stock_writer() {
        let base = ready(LogicalTurn::Clockwise, 0, 11);
        let offer = || Offer::Granted(base.owner.clone());
        for first_exit_is_wake in [true, false] {
            let mut panel = Mock::new(base.clone());
            let mut layer = Layer::new(Usb::Unplugged);
            // A pending record can exist before Launcher ever observed a ready frame.
            panel.ready_valid = false;
            layer.shared_inner(&Ev::Input(Input::Sleep), offer(), &mut panel, false);
            layer.shared_inner(&Ev::Usb(Usb::Drive), offer(), &mut panel, false);
            assert_eq!(panel.minimum(&base.owner), 12);
            if first_exit_is_wake {
                layer.shared_inner(&Ev::Input(Input::Wake), offer(), &mut panel, false);
                assert_eq!(panel.redraws.get(), 0);
                layer.shared_inner(&Ev::Usb(Usb::Unplugged), offer(), &mut panel, false);
            } else {
                layer.shared_inner(&Ev::Usb(Usb::Unplugged), offer(), &mut panel, false);
                assert_eq!(panel.redraws.get(), 0);
                layer.shared_inner(&Ev::Input(Input::Wake), offer(), &mut panel, false);
            }
            layer.shared_inner(&Ev::Input(Input::Wake), offer(), &mut panel, false);
            assert_eq!(panel.redraws.get(), 1);
            assert_eq!(panel.restored, 0);
            panel.ready_valid = true;
            layer.observe(&base.owner, &mut panel, false);
            assert!(layer.current.is_none());
            let fresh = ready(LogicalTurn::Clockwise, 0, 12);
            panel.record = Some(fresh.clone());
            layer.observe(&fresh.owner, &mut panel, false);
            assert_eq!(layer.current, Some(fresh));
        }
    }

    #[test]
    fn custom_sleep_uses_the_configured_portrait_and_refuses_usb_or_changed_geometry() {
        let base = ready(LogicalTurn::Clockwise, 2, 3);
        let mut panel = Mock::new(base.clone());
        let wallpaper: Vec<_> = (0..panel.pixels.len()).map(|i| (i % 197) as u8).collect();
        panel.ready_valid = false;
        panel.storage_exported = true;
        assert!(!paint_sleep_frame(
            &mut panel,
            &base.owner,
            LogicalTurn::UpsideDown,
            wallpaper.clone()
        ));
        assert_eq!(panel.sleep_shows, 0);
        assert!(!panel.stopped);
        panel.storage_exported = false;
        let mut changed = base.clone();
        changed.framebuffer.yoffset += 4;
        panel.after_freeze = Some(changed.clone());
        assert!(!paint_sleep_frame(
            &mut panel,
            &base.owner,
            LogicalTurn::UpsideDown,
            wallpaper.clone()
        ));
        assert_eq!(panel.sleep_shows, 0);
        assert!(!panel.stopped);
        panel.record = Some(base.clone());
        assert!(paint_sleep_frame(
            &mut panel,
            &base.owner,
            LogicalTurn::UpsideDown,
            wallpaper.clone()
        ));
        assert_eq!(
            panel.pixels, wallpaper,
            "upside-down preference matches the firmware rotation"
        );
        panel.after_paint = Some(changed);
        assert!(!paint_sleep_frame(
            &mut panel,
            &base.owner,
            LogicalTurn::UpsideDown,
            wallpaper
        ));
        assert_eq!(panel.sleep_shows, 2);
        assert!(!panel.stopped);
        assert_eq!(panel.redraws.get(), 0);
    }

    #[test]
    fn foreground_exit_invalidates_capture_and_a_replacement_can_start_at_generation_one() {
        let old = ready(LogicalTurn::Clockwise, 2, 22);
        let mut layer = Layer::new(Usb::Unplugged);
        let mut panel = Mock::new(old.clone());
        let _surface = SystemUi::open(&mut layer, &mut panel, &old, &status());
        let queued_tap = gesture(&layer, &old, (234.0, 234.0), (234.0, 234.0));
        let mut new = ready(LogicalTurn::Counterclockwise, 0, 1);
        new.owner = ViewportOwner::parse("v1 43 8747 app-turn.FED789").unwrap();
        panel.record = Some(new.clone());
        panel.stopped = false;
        layer.shared_inner(&Ev::AppEnded, Offer::Legacy, &mut panel, false);
        assert!(!layer.is_open() && !panel.grabbed);
        assert_eq!(panel.restored, 0);
        assert_eq!(panel.redraws.get(), 0);
        layer.observe(&new.owner, &mut panel, false);
        assert_eq!(layer.current, Some(new.clone()));
        assert!(matches!(
            feed(&mut layer, &mut panel, &new, &status(), queued_tap),
            Reaction::Consumed
        ));
    }

    #[test]
    fn generation_watermark_rejects_malformed_or_redirected_files() {
        let root =
            std::env::temp_dir().join(format!("kl-viewport-watermark-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("launcher-occluded");
        assert_eq!(read_minimum(&marker), 0);
        crate::write_atomic(&marker, "42\n").unwrap();
        assert_eq!(read_minimum(&marker), 42);
        for malformed in [
            "42",
            "1 2\n",
            "18446744073709551616\n",
            "9999999999999999999999999\n",
        ] {
            std::fs::write(&marker, malformed).unwrap();
            assert_eq!(read_minimum(&marker), u64::MAX);
        }
        std::fs::remove_file(&marker).unwrap();
        let other = root.join("other-scope");
        std::fs::write(&other, "19\n").unwrap();
        std::os::unix::fs::symlink(other, &marker).unwrap();
        assert_eq!(read_minimum(&marker), u64::MAX);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_scope_uses_legacy_but_pending_and_pending_input_do_not_fall_through() {
        let ready = ready(LogicalTurn::Clockwise, 0, 4);
        let mut panel = Mock::new(ready.clone());
        let mut layer = Layer::new(Usb::Unplugged);
        let status = status();
        let raw = gesture(&layer, &ready, (200.0, 20.0), (200.0, 370.0));
        assert!(matches!(
            layer.native_inner(raw, Offer::Legacy, &mut panel, status.dark_mode, fonts),
            Reaction::Legacy(Input::Swipe { .. })
        ));
        assert!(matches!(
            layer.native_inner(raw, Offer::Invalid, &mut panel, status.dark_mode, fonts),
            Reaction::Consumed
        ));
        panel.record = None;
        assert!(matches!(
            feed(&mut layer, &mut panel, &ready, &status, raw),
            Reaction::Consumed
        ));
        panel.record = Some(ready.clone());
        assert!(matches!(
            feed(&mut layer, &mut panel, &ready, &status, raw),
            Reaction::Consumed
        ));
        assert!(!layer.is_open());
        assert_eq!(panel.shows, 0);
    }
}
