//! Feature-gated Quick Settings over a projected foreground frame.

use std::{
    io,
    path::Path,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    thread,
};

use kl_core::{
    viewport::{LogicalTurn, ViewportTransform},
    Action, DeviceCmd, Input, Layout, Rect, Screen, Status, Ui, Usb,
};
use kl_kindle::{
    display::Display,
    input::{ViewportEvent, ViewportGeneration},
    viewport::{read_ready_viewport, FramebufferGeometry, ReadyViewport, ViewportOwner},
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
    fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport>;
    fn freeze(&mut self, owner: &ViewportOwner, on: bool) -> bool;
    fn snapshot(&self, framebuffer: FramebufferGeometry) -> Option<Vec<u8>>;
    fn restore(&mut self, framebuffer: FramebufferGeometry, pixels: Vec<u8>) -> bool;
    fn redraw_app(&self, owner: &ViewportOwner);
    fn grab_touch(&mut self, on: bool) -> bool;
    fn paint(&mut self, ready: &ReadyViewport, region: Rect, pixels: Vec<u8>) -> bool;
}

pub struct DevicePanel<'a> {
    pub run: &'a Path,
    pub display: &'a mut Display,
}

impl Panel for DevicePanel<'_> {
    fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport> {
        let Offer::Granted(owner) = offer(self.run) else {
            return None;
        };
        let framebuffer = self.display.viewport_framebuffer()?;
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

    fn release(self, panel: &mut impl Panel, restore: bool, redraw: bool, dark: bool) {
        let ready = &self.ready;
        let restored = restore
            && panel.ready(ready.viewport.generation, true).as_ref() == Some(ready)
            && panel.restore(ready.framebuffer, self.pixels);
        if redraw && (!restored || dark != self.dark) {
            panel.redraw_app(&ready.owner);
        }
        panel.freeze(&ready.owner, false);
    }
}

struct Quick {
    captured: Captured,
    ui: Ui,
    renderer: Renderer,
    painted_dark: bool,
}

impl Quick {
    fn paint(&mut self, panel: &mut impl Panel, status: &Status) -> bool {
        self.renderer
            .set_backdrop(self.captured.backdrop(status.dark_mode));
        let pixels = self.renderer.draw(&self.ui, status);
        let area = if self.painted_dark == status.dark_mode {
            self.ui.layout.quick_panel()
        } else {
            self.ui.layout.screen()
        };
        let painted = panel.paint(&self.captured.ready, area, pixels);
        if painted {
            self.painted_dark = status.dark_mode;
        }
        painted
    }
}

pub enum Reaction {
    Legacy(Input),
    Consumed,
    Device(DeviceCmd),
}

pub struct Layer {
    pub generation: Arc<ViewportGeneration>,
    latest: Option<ReadyViewport>,
    current: Option<ReadyViewport>,
    quick: Option<Quick>,
    asleep: bool,
    usb: Usb,
    /// A stock writer invalidates the previous app generation even after wake or eject.
    external_write: bool,
    bound_owner: Option<ViewportOwner>,
    scoped: bool,
}

impl Layer {
    pub fn new(usb: Usb) -> Self {
        Self {
            generation: Arc::new(ViewportGeneration::default()),
            latest: None,
            current: None,
            quick: None,
            asleep: false,
            usb,
            external_write: false,
            bound_owner: None,
            scoped: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.quick.is_some()
    }

    fn select(&mut self, candidate: Option<ReadyViewport>) {
        if self.current != candidate {
            self.current = candidate;
            self.generation.advance();
        }
    }

    fn observe(&mut self, owner: &ViewportOwner, panel: &impl Panel) {
        if self
            .bound_owner
            .as_ref()
            .is_some_and(|previous| previous != owner)
        {
            self.reset_owner();
        }
        self.bound_owner = Some(owner.clone());
        self.scoped = true;
        if self.asleep || self.usb == Usb::Drive {
            self.select(None);
            return;
        }
        let minimum = if self.external_write {
            // If the previous visible generation is unknown, wait for a new app run.
            let Some(next) = self
                .latest
                .as_ref()
                .and_then(|r| r.viewport.generation.checked_add(1))
            else {
                self.select(None);
                return;
            };
            next
        } else {
            self.latest.as_ref().map_or(0, |r| r.viewport.generation)
        };
        let observed = panel
            .ready(minimum, false)
            .filter(|ready| &ready.owner == owner);
        if let Some(ready) = &observed {
            self.latest = Some(ready.clone());
            self.external_write = false;
        }
        self.select(observed);
    }

    fn close(&mut self, panel: &mut impl Panel, restore: bool, redraw: bool, dark: bool) {
        if let Some(quick) = self.quick.take() {
            panel.grab_touch(false);
            quick.captured.release(panel, restore, redraw, dark);
            self.generation.advance();
        }
    }

    fn reset_owner(&mut self) {
        self.bound_owner = None;
        self.scoped = false;
        self.latest = None;
        self.current = None;
        self.external_write = false;
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
        match event {
            Ev::AppEnded => {
                self.close(panel, false, false, dark);
                self.reset_owner();
            }
            Ev::Usb(usb) if *usb != self.usb => {
                let was_drive = self.usb == Usb::Drive;
                self.usb = *usb;
                if *usb == Usb::Drive && scoped {
                    self.stock_wrote(panel, dark);
                } else if was_drive {
                    if let Offer::Granted(owner) = offer {
                        panel.redraw_app(&owner);
                    }
                }
            }
            Ev::Input(Input::Sleep) => {
                self.asleep = true;
                if scoped {
                    self.stock_wrote(panel, dark);
                    return true;
                }
            }
            Ev::Input(Input::Wake) => {
                self.asleep = false;
                if scoped {
                    if let Offer::Granted(owner) = offer {
                        panel.redraw_app(&owner);
                    }
                    return true;
                }
            }
            Ev::Tick => {
                if let Offer::Granted(owner) = offer {
                    self.observe(&owner, panel);
                } else if scoped {
                    self.select(None);
                    self.close(panel, false, true, dark);
                }
            }
            _ => {}
        }
        false
    }

    fn stock_wrote(&mut self, panel: &mut impl Panel, dark: bool) {
        self.external_write = true;
        self.select(None);
        self.close(panel, false, false, dark);
    }

    pub fn native(
        &mut self,
        event: ViewportEvent,
        panel: &mut DevicePanel<'_>,
        status: &Status,
        fonts: impl FnOnce() -> Vec<String>,
    ) -> Reaction {
        self.native_inner(event, offer(panel.run), panel, status, fonts)
    }

    fn native_inner(
        &mut self,
        event: ViewportEvent,
        offer: Offer,
        panel: &mut impl Panel,
        status: &Status,
        fonts: impl FnOnce() -> Vec<String>,
    ) -> Reaction {
        self.scoped |= offer != Offer::Legacy;
        if offer == Offer::Legacy {
            if self.scoped {
                self.close(panel, false, true, status.dark_mode);
                return Reaction::Consumed;
            }
            return event
                .resolve(&self.generation, LogicalTurn::Upright)
                .map_or(Reaction::Consumed, |input| Reaction::Legacy(input.input));
        }
        let Offer::Granted(owner) = offer else {
            self.close(panel, false, true, status.dark_mode);
            return Reaction::Consumed;
        };
        self.observe(&owner, panel);
        let Some(ready) = self.current.clone() else {
            self.close(panel, false, true, status.dark_mode);
            return Reaction::Consumed;
        };
        if self
            .quick
            .as_ref()
            .is_some_and(|quick| quick.captured.ready != ready)
        {
            self.close(panel, false, true, status.dark_mode);
            return Reaction::Consumed;
        }
        let Some(resolved) = event.resolve(&self.generation, ready.viewport.turn) else {
            return Reaction::Consumed;
        };
        if let Some(quick) = self.quick.as_mut() {
            let action = quick.ui.handle(resolved.input);
            return match quick.ui.screen {
                Screen::Home => {
                    self.close(panel, true, true, status.dark_mode);
                    Reaction::Consumed
                }
                Screen::QuickSettings => match action {
                    Action::Device(cmd) => Reaction::Device(cmd),
                    _ => Reaction::Consumed,
                },
                _ => {
                    // Full Settings and power dialogs have separate session lifetimes.
                    quick.ui.screen = Screen::QuickSettings;
                    Reaction::Consumed
                }
            };
        }
        let (w, h) = ready.viewport.logical_size;
        let layout = Layout::new(w, h, 4, 5);
        if !system_layer::opens_quick_settings(resolved.input, layout.status().bottom()) {
            return Reaction::Consumed;
        }
        let Ok(renderer) = Renderer::new(w, h, &fonts()) else {
            return Reaction::Consumed;
        };
        let Some(captured) = Captured::freeze(panel, &ready, status.dark_mode) else {
            return Reaction::Consumed;
        };
        if !panel.grab_touch(true) {
            captured.release(panel, true, true, status.dark_mode);
            return Reaction::Consumed;
        }
        let mut ui = Ui::new(Vec::new(), layout);
        ui.screen = Screen::QuickSettings;
        ui.display_support = status.display_support;
        self.quick = Some(Quick {
            captured,
            ui,
            renderer,
            painted_dark: status.dark_mode,
        });
        self.generation.advance();
        self.paint_inner(panel, status);
        Reaction::Consumed
    }

    pub fn paint(&mut self, panel: &mut DevicePanel<'_>, status: &Status) {
        self.paint_inner(panel, status);
    }

    fn paint_inner(&mut self, panel: &mut impl Panel, status: &Status) {
        let failed = self.quick.as_mut().is_some_and(|quick| {
            let ready = &quick.captured.ready;
            panel.ready(ready.viewport.generation, true).as_ref() != Some(ready)
                || !quick.paint(panel, status)
        });
        if failed {
            self.close(panel, false, true, status.dark_mode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        stopped: bool,
        grabbed: bool,
        original: Vec<u8>,
        pixels: Vec<u8>,
        snapshot_reads: Cell<usize>,
        restored: usize,
        shows: usize,
    }

    impl Mock {
        fn new(ready: ReadyViewport) -> Self {
            let (width, height) = ready.framebuffer.native_size;
            let original: Vec<_> = (0..width * height).map(|i| (i % 223) as u8).collect();
            Self {
                record: Some(ready),
                after_freeze: None,
                after_capture: None,
                stopped: false,
                grabbed: false,
                pixels: original.clone(),
                original,
                snapshot_reads: Cell::new(0),
                restored: 0,
                shows: 0,
            }
        }
    }

    impl Panel for Mock {
        fn ready(&self, minimum: u64, stopped: bool) -> Option<ReadyViewport> {
            if stopped && !self.stopped {
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
            true
        }

        fn redraw_app(&self, _owner: &ViewportOwner) {}

        fn grab_touch(&mut self, on: bool) -> bool {
            self.grabbed = on;
            true
        }

        fn paint(&mut self, ready: &ReadyViewport, region: Rect, pixels: Vec<u8>) -> bool {
            if !self.stopped || !self.grabbed || self.record.as_ref() != Some(ready) {
                return false;
            }
            let transform = transform(ready);
            let logical = transform.unproject(self.original.clone()).unwrap();
            let first_uncovered = region.bottom() as usize * ready.viewport.logical_size.0 as usize;
            assert_eq!(pixels[first_uncovered..], logical[first_uncovered..]);
            self.pixels = transform.project(pixels).unwrap();
            self.shows += 1;
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
            status,
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
                layer.observe(&ready.owner, &panel);
                let (w, h) = ready.viewport.logical_size;
                let x = w as f32 / 2.0;
                let bottom = h as f32 - 20.0;
                let swipe_bottom = gesture(&layer, &ready, (x, bottom), (x, bottom - 350.0));
                feed(&mut layer, &mut panel, &ready, &status, swipe_bottom);
                assert!(!layer.is_open());
                let swipe_top = gesture(&layer, &ready, (x, 20.0), (x, 370.0));
                feed(&mut layer, &mut panel, &ready, &status, swipe_top);
                assert!(layer.is_open() && panel.stopped && panel.grabbed);
                assert_ne!(panel.pixels, panel.original);
                assert_eq!(panel.shows, 1);

                let plus = layer.quick.as_ref().unwrap().ui.layout.quick_plus(0);
                let point = ((plus.x + plus.w / 2) as f32, (plus.y + plus.h / 2) as f32);
                let tap = gesture(&layer, &ready, point, point);
                assert!(matches!(
                    feed(&mut layer, &mut panel, &ready, &status, tap),
                    Reaction::Device(DeviceCmd::Light(1))
                ));
                let swaps = matches!(turn, LogicalTurn::Clockwise | LogicalTurn::UpsideDown);
                let back = if swaps {
                    profile::ACTIVE.page_forward_key
                } else {
                    profile::ACTIVE.page_back_key
                };
                let forward = if swaps {
                    profile::ACTIVE.page_back_key
                } else {
                    profile::ACTIVE.page_forward_key
                };
                let epoch = layer.generation.current();
                feed(
                    &mut layer,
                    &mut panel,
                    &ready,
                    &status,
                    ViewportEvent {
                        generation: epoch,
                        native: NativeInput::PageKey(forward),
                    },
                );
                assert!(layer.is_open());
                feed(
                    &mut layer,
                    &mut panel,
                    &ready,
                    &status,
                    ViewportEvent {
                        generation: epoch,
                        native: NativeInput::PageKey(back),
                    },
                );
                assert!(!layer.is_open() && !panel.stopped && !panel.grabbed);
                assert_eq!(panel.restored, 1);
                assert_eq!(panel.pixels, panel.original);
            }
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
            layer.observe(&old.owner, &panel);
            let swipe = gesture(&layer, &old, (200.0, 20.0), (200.0, 370.0));
            feed(&mut layer, &mut panel, &old, &status, swipe);
            assert!(layer.is_open());
            layer.shared_inner(&enter, Offer::Granted(old.owner.clone()), &mut panel, false);
            assert!(!layer.is_open() && !panel.grabbed && !panel.stopped);
            assert_eq!(panel.restored, 0);
            layer.shared_inner(&exit, Offer::Granted(old.owner.clone()), &mut panel, false);
            layer.observe(&old.owner, &panel);
            assert!(layer.current.is_none());
            let blocked_swipe = gesture(&layer, &old, (200.0, 20.0), (200.0, 370.0));
            feed(&mut layer, &mut panel, &old, &status, blocked_swipe);
            assert_eq!(panel.shows, 1);
            panel.record = Some(newer.clone());
            layer.observe(&newer.owner, &panel);
            assert_eq!(layer.current, Some(newer));
        }
    }

    #[test]
    fn missing_scope_uses_legacy_but_pending_and_pending_input_do_not_fall_through() {
        let ready = ready(LogicalTurn::Clockwise, 0, 4);
        let mut panel = Mock::new(ready.clone());
        let mut layer = Layer::new(Usb::Unplugged);
        let status = status();
        let raw = gesture(&layer, &ready, (200.0, 20.0), (200.0, 370.0));
        assert!(matches!(
            layer.native_inner(raw, Offer::Legacy, &mut panel, &status, fonts),
            Reaction::Legacy(Input::Swipe { .. })
        ));
        assert!(matches!(
            layer.native_inner(raw, Offer::Invalid, &mut panel, &status, fonts),
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
