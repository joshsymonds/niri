use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::mem;
use std::time::Duration;

use anyhow::Context as _;
use calloop::LoopHandle;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Bind;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::gbm::Modifier;
use smithay::utils::{Physical, Point, Scale, Size};
use zbus::object_server::SignalEmitter;

use crate::dbus::mutter_screen_cast::{self, CursorMode, ScreenCastToNiri, StreamTargetId};
use crate::niri::{CastTarget, Niri, OutputRenderElements, PointerRenderElements, State};
use crate::niri_render_elements;
use crate::render_helpers::{RenderCtx, RenderTarget};
use crate::utils::{get_monotonic_time, CastSessionId, CastStreamId};
use crate::window::mapped::{MappedId, WindowCastRenderElements};

mod active_casts;
mod pw_utils;
pub use active_casts::ActiveCasts;
use pw_utils::{allocate_dmabuf, Cast, CastSizeChange, CursorData, PipeWire, PwToNiri};

pub struct Screencasting {
    pub casts: Vec<Cast>,

    /// Dynamic-target casts waiting for their first target to start.
    pub pending_dynamic_casts: Vec<PendingCast>,

    /// Derived snapshot of which outputs and window-ids are currently being
    /// cast. Recomputed via [`Self::recompute_active_casts`] whenever the cast
    /// set changes (start, target switch, stop). Read by the indicator border,
    /// layer-hiding, and Zoom auto-hide subsystems.
    pub active_casts: ActiveCasts,

    /// Shared map of last-known physical-pixel window sizes keyed by window
    /// id, surfaced to xdg-desktop-portal consumers via
    /// `org.gnome.Mutter.ScreenCast.Stream.parameters`. Refreshed every
    /// `State::refresh` from the live layout so consumers (notably Zoom) get
    /// a usable size hint instead of the `(1, 1)` stub that previously
    /// caused window casts to render as 1×1 / blank on the consumer side.
    /// Physical pixels match the units the consumer's PipeWire stream uses
    /// for its negotiated buffer geometry, so the two stay consistent.
    pub window_cast_sizes: mutter_screen_cast::WindowCastSizes,

    pub pw_to_niri: calloop::channel::Sender<PwToNiri>,

    /// Screencast output for each mapped window.
    pub mapped_cast_output: HashMap<Window, Output>,

    /// Window ID for the "dynamic cast" special window for the xdp-gnome picker.
    pub dynamic_cast_id_for_portal: MappedId,

    // Drop PipeWire last, and specifically after casts, to prevent a double-free (yay).
    pub pipewire: Option<PipeWire>,

    /// Cached result of probing whether the primary renderer can bind an implicit-modifier
    /// (`Modifier::Invalid`) GBM dmabuf as a render target.
    ///
    /// `None` until the probe has run once; probed at most once per compositor run.
    implicit_modifier_renderable: Option<bool>,
}

/// A screencast request that hasn't been started yet.
pub struct PendingCast {
    pub session_id: CastSessionId,
    pub stream_id: CastStreamId,
    pub cursor_mode: CursorMode,
    pub signal_ctx: SignalEmitter<'static>,
}

impl Screencasting {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Self {
        let pw_to_niri = {
            let (pw_to_niri, from_pipewire) = calloop::channel::channel();
            event_loop
                .insert_source(from_pipewire, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => state.on_pw_msg(msg),
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            pw_to_niri
        };

        Self {
            casts: vec![],
            pending_dynamic_casts: vec![],
            active_casts: ActiveCasts::default(),
            window_cast_sizes: Default::default(),
            pw_to_niri,
            mapped_cast_output: HashMap::new(),
            dynamic_cast_id_for_portal: MappedId::next(),
            pipewire: None,
            implicit_modifier_renderable: None,
        }
    }

    /// Refresh [`Self::active_casts`] from the current contents of
    /// [`Self::casts`]. Call after any change to the cast set or to any cast's
    /// target.
    pub fn recompute_active_casts(&mut self) {
        self.active_casts
            .recompute_from_targets(self.casts.iter().map(|cast| &cast.target));
    }
}

impl State {
    fn prepare_pw_cast(&mut self) -> anyhow::Result<(GbmDevice<DrmDeviceFd>, FormatSet)> {
        let gbm = self
            .backend
            .gbm_device()
            .context("no GBM device available")?;

        // Ensure PipeWire is initialized.
        if self.niri.casting.pipewire.is_none() {
            let pw = PipeWire::new(
                self.niri.event_loop.clone(),
                self.niri.casting.pw_to_niri.clone(),
            )
            .context("error initializing PipeWire")?;
            self.niri.casting.pipewire = Some(pw);
        }

        let cached_probe = self.niri.casting.implicit_modifier_renderable;
        let (mut render_formats, probe_result) = self
            .backend
            .with_primary_renderer(|renderer| {
                let formats = renderer.egl_context().dmabuf_render_formats().clone();

                // Probe at most once per compositor run, and only if there's actually an
                // implicit-modifier entry that might need stripping.
                let probe_result = cached_probe.or_else(|| {
                    formats
                        .iter()
                        .any(|f| f.modifier == Modifier::Invalid)
                        .then(|| probe_implicit_modifier_renderable(renderer, &gbm))
                });

                (formats, probe_result)
            })
            .unwrap_or_default();

        if let Some(result) = probe_result {
            if cached_probe.is_none() {
                debug!("implicit modifier dmabuf renderable: {result}");
            }
            self.niri.casting.implicit_modifier_renderable = Some(result);
        }

        let force_invalid_modifier = {
            let config = self.niri.config.borrow();
            config.debug.force_pipewire_invalid_modifier
        };

        if force_invalid_modifier {
            render_formats = render_formats
                .into_iter()
                .filter(|f| f.modifier == Modifier::Invalid)
                .collect();
        }

        render_formats = strip_unsupported_implicit_modifier(
            render_formats,
            probe_result,
            force_invalid_modifier,
        );

        Ok((gbm, render_formats))
    }

    pub fn on_pw_msg(&mut self, msg: PwToNiri) {
        match msg {
            PwToNiri::StopCast { session_id } => self.niri.stop_cast(session_id),
            PwToNiri::Redraw { stream_id } => self.redraw_cast(stream_id),
            PwToNiri::FatalError => {
                warn!("stopping PipeWire due to fatal error");
                let casting = &mut self.niri.casting;
                if let Some(pw) = casting.pipewire.take() {
                    let mut ids = HashSet::new();
                    for cast in &casting.pending_dynamic_casts {
                        ids.insert(cast.session_id);
                    }
                    for cast in &casting.casts {
                        ids.insert(cast.session_id);
                    }
                    for id in ids {
                        self.niri.stop_cast(id);
                    }
                    self.niri.event_loop.remove(pw.token);
                }
            }
        }
    }

    fn redraw_cast(&mut self, stream_id: CastStreamId) {
        let _span = tracy_client::span!("State::redraw_cast");

        let locked = self.niri.is_locked();

        let casts = &mut self.niri.casting.casts;
        let Some(idx) = casts.iter().position(|cast| cast.stream_id == stream_id) else {
            warn!("cast to redraw is missing");
            return;
        };
        let cast = &mut casts[idx];

        let id = match &cast.target {
            CastTarget::Nothing => {
                self.backend.with_primary_renderer(|renderer| {
                    if cast.dequeue_buffer_and_clear(renderer) {
                        cast.last_frame_time = get_monotonic_time();
                    }
                });
                return;
            }
            CastTarget::Output { output, .. } => {
                if let Some(output) = output.upgrade() {
                    self.niri.queue_redraw(&output);
                }
                return;
            }
            CastTarget::Window { id } => *id,
        };

        // Privacy: while the session is locked, window screencasts must not
        // leak real window content. Output casts are gated inside render_inner;
        // this path renders the window directly and bypasses that. Clear the
        // buffer (like CastTarget::Nothing) so the stream stays alive and
        // resumes automatically on unlock.
        if locked {
            self.backend.with_primary_renderer(|renderer| {
                if cast.dequeue_buffer_and_clear(renderer) {
                    cast.last_frame_time = get_monotonic_time();
                }
            });
            return;
        }

        // Lack of partial borrowing strikes again...
        let mut casts = mem::take(&mut self.niri.casting.casts);
        let cast = &mut casts[idx];
        let mut stop = false;
        // Use a loop {} so we can break instead of early-return.
        #[allow(clippy::never_loop)]
        loop {
            let mut windows = self.niri.layout.windows();
            let Some((_, mapped)) = windows.find(|(_, mapped)| mapped.id().get() == id) else {
                break;
            };

            // Use the cached output since it will be present even if the output was
            // currently disconnected.
            let Some(output) = self.niri.casting.mapped_cast_output.get(&mapped.window) else {
                break;
            };

            let scale = Scale::from(output.current_scale().fractional_scale());
            let bbox = mapped
                .window
                .bbox_with_popups()
                .to_physical_precise_up(scale);

            match cast.ensure_size(bbox.size) {
                Ok(CastSizeChange::Ready) => (),
                Ok(CastSizeChange::Pending) => break,
                Err(err) => {
                    warn!("error updating stream size, stopping screencast: {err:?}");
                    stop = true;
                    break;
                }
            }

            let cast_failed = self.backend.with_primary_renderer(|renderer| {
                let mut elements = Vec::new();
                let mut pointer_location = Point::default();

                if self.niri.pointer_visibility.is_visible() {
                    if let Some((pointer_pos, win_pos)) =
                        self.niri.pointer_pos_for_window_cast(mapped)
                    {
                        // Pointer location must be relative to the screencast buffer.
                        // - win_pos is the position of the main window surface in output-local
                        //   coordinates
                        // - bbox.loc moves us relative to the screencast buffer
                        let buf_pos = win_pos + bbox.loc.to_f64().to_logical(scale);
                        let output_pos =
                            self.niri.global_space.output_geometry(output).unwrap().loc;
                        pointer_location = pointer_pos - output_pos.to_f64() - buf_pos;

                        let pos = buf_pos.to_physical_precise_round(scale).upscale(-1);
                        self.niri.render_pointer(renderer, output, &mut |elem| {
                            let elem =
                                RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                            elements.push(CastRenderElement::from(elem));
                        });
                    }
                }

                let main_start = elements.len();
                mapped.render_for_screen_cast(renderer, scale, &mut |elem| {
                    elements.push(CastRenderElement::from(elem))
                });

                let cursor_data =
                    CursorData::compute(&elements, main_start, pointer_location, scale);

                match cast.dequeue_buffer_and_render(
                    renderer,
                    &elements,
                    &cursor_data,
                    bbox.size,
                    scale,
                ) {
                    Ok(true) => {
                        cast.last_frame_time = get_monotonic_time();
                        false
                    }
                    Ok(false) => false,
                    Err(err) => {
                        warn!("error rendering cast, stopping screencast: {err:?}");
                        true
                    }
                }
            });

            if cast_failed == Some(true) {
                stop = true;
            }

            break;
        }
        let session_id = cast.session_id;
        self.niri.casting.casts = casts;

        if stop {
            self.niri.stop_cast(session_id);
        }
    }

    pub fn set_dynamic_cast_target(&mut self, target: CastTarget) {
        let _span = tracy_client::span!("State::set_dynamic_cast_target");

        let mut refresh = None;
        match &target {
            // Leave refresh as is when clearing. Chances are, the next refresh will match it,
            // then we'll avoid reconfiguring.
            CastTarget::Nothing => (),
            CastTarget::Output { output, .. } => {
                if let Some(output) = output.upgrade() {
                    refresh = Some(output.current_mode().unwrap().refresh as u32);
                }
            }
            CastTarget::Window { id } => {
                let mut windows = self.niri.layout.windows();
                if let Some((_, mapped)) = windows.find(|(_, mapped)| mapped.id().get() == *id) {
                    if let Some(output) = self.niri.casting.mapped_cast_output.get(&mapped.window) {
                        refresh = Some(output.current_mode().unwrap().refresh as u32);
                    }
                }
            }
        }

        let mut to_redraw = Vec::new();
        let mut to_stop = Vec::new();
        for cast in &mut self.niri.casting.casts {
            if !cast.dynamic_target {
                continue;
            }

            if let Some(refresh) = refresh {
                if let Err(err) = cast.set_refresh(refresh) {
                    warn!("error changing cast FPS: {err:?}");
                    to_stop.push(cast.session_id);
                    continue;
                }
            }

            cast.target = target.clone();
            to_redraw.push(cast.stream_id);
        }

        self.niri.casting.recompute_active_casts();

        for id in to_redraw {
            self.redraw_cast(id);
        }

        // Start any pending dynamic casts if we have a real target.
        if !matches!(target, CastTarget::Nothing) {
            self.start_pending_dynamic_casts(&target);
        }
    }

    fn start_pending_dynamic_casts(&mut self, target: &CastTarget) {
        let pending = &self.niri.casting.pending_dynamic_casts;
        if pending.is_empty() {
            return;
        }
        debug!("starting {} pending dynamic cast(s)", pending.len());

        let _span = tracy_client::span!("State::start_pending_dynamic_casts");

        // We don't stop dynamic casts on missing output/window.
        let (size, refresh) = match target {
            CastTarget::Nothing => panic!("dynamic cast starting target must not be Nothing"),
            CastTarget::Output { output, .. } => {
                let Some(output) = output.upgrade() else {
                    return;
                };
                cast_params_for_output(&output)
            }
            CastTarget::Window { id } => {
                let Some((size, refresh)) = self.niri.cast_params_for_window(*id) else {
                    return;
                };
                (size, refresh)
            }
        };

        let (gbm, render_formats) = match self.prepare_pw_cast() {
            Ok(x) => x,
            Err(err) => {
                warn!("error starting pending screencasts: {err:?}");
                let mut ids = HashSet::new();
                for pending in self.niri.casting.pending_dynamic_casts.drain(..) {
                    ids.insert(pending.session_id);
                }
                for id in ids {
                    self.niri.stop_cast(id);
                }
                return;
            }
        };
        let pw = self.niri.casting.pipewire.as_ref().unwrap();

        // Alpha is always true since the dynamic target can change between window & output.
        let alpha = true;

        // Start each pending cast.
        let mut to_stop = HashSet::new();
        for pending in self.niri.casting.pending_dynamic_casts.drain(..) {
            let res = pw.start_cast(
                gbm.clone(),
                render_formats.clone(),
                self.niri.casting.implicit_modifier_renderable,
                pending.session_id,
                pending.stream_id,
                target.clone(),
                size,
                refresh,
                alpha,
                pending.cursor_mode,
                pending.signal_ctx,
            );
            match res {
                Ok(mut cast) => {
                    cast.dynamic_target = true;
                    self.niri.casting.casts.push(cast);
                }
                Err(err) => {
                    warn!("error starting pending screencast: {err:?}");
                    to_stop.insert(pending.session_id);
                }
            }
        }

        self.niri.casting.recompute_active_casts();

        for session_id in to_stop {
            self.niri.stop_cast(session_id);
        }
    }

    pub fn on_screen_cast_msg(&mut self, msg: ScreenCastToNiri) {
        match msg {
            ScreenCastToNiri::StartCast {
                session_id,
                stream_id,
                target,
                cursor_mode,
                signal_ctx,
            } => {
                let _span = tracy_client::span!("StartCast");
                let _span = debug_span!("StartCast", %session_id, %stream_id).entered();

                let (target, size, refresh, alpha) = match target {
                    StreamTargetId::Output { name } => {
                        let global_space = &self.niri.global_space;
                        let output = global_space.outputs().find(|out| out.name() == name);
                        let Some(output) = output else {
                            warn!("error starting screencast: requested output is missing");
                            self.niri.stop_cast(session_id);
                            return;
                        };

                        let (size, refresh) = cast_params_for_output(output);
                        (CastTarget::output(output), size, refresh, false)
                    }
                    StreamTargetId::Window { id }
                        if id == self.niri.casting.dynamic_cast_id_for_portal.get() =>
                    {
                        debug!("delaying dynamic cast until target is set");
                        self.niri.casting.pending_dynamic_casts.push(PendingCast {
                            session_id,
                            stream_id,
                            cursor_mode,
                            signal_ctx,
                        });
                        return;
                    }
                    StreamTargetId::Window { id } => {
                        let Some((size, refresh)) = self.niri.cast_params_for_window(id) else {
                            warn!("error starting screencast: requested window is missing");
                            self.niri.stop_cast(session_id);
                            return;
                        };
                        (CastTarget::Window { id }, size, refresh, true)
                    }
                };

                let (gbm, render_formats) = match self.prepare_pw_cast() {
                    Ok(x) => x,
                    Err(err) => {
                        warn!("error starting screencast: {err:?}");
                        self.niri.stop_cast(session_id);
                        return;
                    }
                };
                let pw = self.niri.casting.pipewire.as_ref().unwrap();

                let res = pw.start_cast(
                    gbm,
                    render_formats,
                    self.niri.casting.implicit_modifier_renderable,
                    session_id,
                    stream_id,
                    target,
                    size,
                    refresh,
                    alpha,
                    cursor_mode,
                    signal_ctx,
                );
                match res {
                    Ok(cast) => {
                        self.niri.casting.casts.push(cast);
                        self.niri.casting.recompute_active_casts();
                    }
                    Err(err) => {
                        warn!("error starting screencast: {err:?}");
                        self.niri.stop_cast(session_id);
                    }
                }
            }
            ScreenCastToNiri::StopCast { session_id } => self.niri.stop_cast(session_id),
        }
    }
}

impl Niri {
    pub fn refresh_mapped_cast_window_rules(&mut self) {
        // O(N^2) but should be fine since there aren't many casts usually.
        self.layout.with_windows_mut(|mapped, _| {
            let id = mapped.id().get();
            // Find regardless of cast.is_active.
            let value = self
                .casting
                .casts
                .iter()
                .any(|cast| cast.target == (CastTarget::Window { id }));
            mapped.set_is_window_cast_target(value);
        });
    }

    /// Refresh `Screencasting::window_cast_sizes` for every window currently
    /// being cast, so the DBus `Stream::parameters` property can report a
    /// sensible physical-pixel size to xdg-desktop-portal consumers (Zoom in
    /// particular — see `mutter_screen_cast::WindowCastSizes`). Physical
    /// pixels match the consumer's PipeWire stream geometry so the two stay
    /// in agreement; the Mutter protocol nominally expects logical, but
    /// PipeWire-aware consumers reconcile via the stream params anyway and a
    /// matching unit eliminates one source of drift.
    ///
    /// The map only holds sizes for windows we have an active cast for; old
    /// entries are pruned each call.
    pub fn refresh_window_cast_sizes(&mut self) {
        let active_windows = self.casting.active_casts.windows.clone();
        let mut new_sizes: HashMap<u64, (i32, i32)> = HashMap::new();
        for id in &active_windows {
            if let Some((size_phys, _)) = self.cast_params_for_window(*id) {
                new_sizes.insert(*id, (size_phys.w, size_phys.h));
            }
        }
        if let Ok(mut map) = self.casting.window_cast_sizes.lock() {
            *map = new_sizes;
        }
    }

    /// Set or clear the Zoom auto-hide flag on each window based on:
    /// - the user's `screen-cast.hide-zoom-non-shared-windows` config flag,
    /// - whether any cast is currently active,
    /// - whether the window's app-id (Wayland) or `WM_CLASS` (xwayland, exposed via xdg-shell
    ///   app-id by xwayland-satellite) matches `zoom`/`Zoom`,
    /// - and whether the window is NOT itself the cast target (so the window the user is actively
    ///   sharing is never hidden from its own cast).
    ///
    /// Runs every `State::refresh` next to [`Self::refresh_mapped_cast_window_rules`].
    pub fn refresh_screencast_auto_hide(&mut self) {
        let config = self.config.borrow();
        let enabled = config.screen_cast.hide_zoom_non_shared_windows;
        let cast_active = !self.casting.active_casts.is_empty();
        // Drop the borrow so `with_windows_mut` can re-borrow Niri internals.
        drop(config);

        let mut changed = false;

        if !enabled || !cast_active {
            self.layout.with_windows_mut(|mapped, _| {
                changed |= mapped.set_block_out_for_screencast_auto(false);
            });
        } else {
            let active_window_ids: HashSet<u64> = self.casting.active_casts.windows.clone();
            self.layout.with_windows_mut(|mapped, _| {
                let id = mapped.id().get();
                if active_window_ids.contains(&id) {
                    // This window IS being cast — never hide it from its own cast.
                    changed |= mapped.set_block_out_for_screencast_auto(false);
                    return;
                }
                // Run the predicate inside the toplevel-role closure so we avoid
                // cloning the app_id String on the State::refresh hot path. The
                // closure runs under the XdgToplevelSurfaceData mutex; the
                // predicate is a cheap `matches!` on a &str borrow.
                let is_zoom = crate::utils::with_toplevel_role(mapped.toplevel(), |role| {
                    role.app_id.as_deref().is_some_and(is_zoom_app_id)
                });
                changed |= mapped.set_block_out_for_screencast_auto(is_zoom);
            });
        }

        // A flag flip changes cast renders without damaging anything on its own; queue a
        // redraw so active casts pick up the new exclusion state instead of keeping a stale
        // frame until the next damage.
        if changed {
            self.queue_redraw_all();
        }
    }

    pub fn refresh_mapped_cast_outputs(&mut self) {
        let mut seen = HashSet::new();
        let mut output_changed = vec![];

        self.layout.with_windows(|mapped, output, _, _| {
            seen.insert(mapped.window.clone());

            let Some(output) = output else {
                return;
            };

            match self.casting.mapped_cast_output.entry(mapped.window.clone()) {
                Entry::Occupied(mut entry) => {
                    if entry.get() != output {
                        entry.insert(output.clone());
                        output_changed.push((mapped.id(), output.clone()));
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(output.clone());
                }
            }
        });

        self.casting
            .mapped_cast_output
            .retain(|win, _| seen.contains(win));

        let mut to_stop = vec![];
        for (id, out) in output_changed {
            let refresh = out.current_mode().unwrap().refresh as u32;
            let target = CastTarget::Window { id: id.get() };
            for cast in self
                .casting
                .casts
                .iter_mut()
                .filter(|cast| cast.target == target)
            {
                if let Err(err) = cast.set_refresh(refresh) {
                    warn!("error changing cast FPS: {err:?}");
                    to_stop.push(cast.session_id);
                };
            }
        }

        for session_id in to_stop {
            self.stop_cast(session_id);
        }
    }

    pub fn render_for_screen_cast(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        target_presentation_time: Duration,
    ) {
        let _span = tracy_client::span!("Niri::render_for_screen_cast");

        let weak = output.downgrade();
        let size = output.current_mode().unwrap().size;
        let transform = output.current_transform();
        let size = transform.transform_size(size);

        let scale = Scale::from(output.current_scale().fractional_scale());

        let mut elements = Vec::new();
        let mut cursor_data = None;

        let mut casts_to_stop = vec![];

        let mut casts = mem::take(&mut self.casting.casts);
        for cast in &mut casts {
            if !cast.is_active() {
                continue;
            }

            if !cast.target.matches_output(&weak) {
                continue;
            }

            match cast.ensure_size(size) {
                Ok(CastSizeChange::Ready) => (),
                Ok(CastSizeChange::Pending) => continue,
                Err(err) => {
                    warn!("error updating stream size, stopping screencast: {err:?}");
                    casts_to_stop.push(cast.session_id);
                }
            }

            if cast.check_time_and_schedule(output, target_presentation_time) {
                continue;
            }

            if cursor_data.is_none() {
                let mut pointer_pos = Point::default();
                if self.pointer_visibility.is_visible() {
                    let output_geo = self.global_space.output_geometry(output).unwrap().to_f64();
                    let pointer_loc = self
                        .tablet_cursor_location
                        .unwrap_or_else(|| self.seat.get_pointer().unwrap().current_location());
                    // Only render when the pointer is within the output. Otherwise, it will
                    // happily appear anywhere outside the output video source in OBS.
                    if output_geo.contains(pointer_loc) {
                        pointer_pos = pointer_loc - output_geo.loc;
                        self.render_pointer(renderer, output, &mut |elem| {
                            elements.push(elem.into())
                        });
                    }
                }

                let main_start = elements.len();
                let ctx = RenderCtx {
                    renderer,
                    target: RenderTarget::Screencast,
                    xray: None,
                };
                self.render(ctx, output, false, &mut |elem| elements.push(elem.into()));

                cursor_data = Some(CursorData::compute(
                    &elements,
                    main_start,
                    pointer_pos,
                    scale,
                ));
            }
            let cursor_data = cursor_data.as_ref().unwrap();

            match cast.dequeue_buffer_and_render(renderer, &elements, cursor_data, size, scale) {
                Ok(true) => cast.last_frame_time = target_presentation_time,
                Ok(false) => (),
                Err(err) => {
                    warn!("error rendering cast, stopping screencast: {err:?}");
                    casts_to_stop.push(cast.session_id);
                }
            }
        }
        self.casting.casts = casts;

        for id in casts_to_stop {
            self.stop_cast(id);
        }
    }

    pub fn render_windows_for_screen_cast(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        target_presentation_time: Duration,
    ) {
        let _span = tracy_client::span!("Niri::render_windows_for_screen_cast");

        let scale = Scale::from(output.current_scale().fractional_scale());

        let mut casts_to_stop = vec![];

        let mut casts = mem::take(&mut self.casting.casts);
        for cast in &mut casts {
            if !cast.is_active() {
                continue;
            }

            let CastTarget::Window { id } = cast.target else {
                continue;
            };

            // Privacy: while the session is locked, window screencasts must not
            // leak real window content. Output casts are gated inside
            // render_inner (Niri::render); window casts render the window
            // directly and bypass that. Clear the buffer so the stream stays
            // alive and resumes automatically on unlock.
            if self.is_locked() {
                if cast.dequeue_buffer_and_clear(renderer) {
                    cast.last_frame_time = target_presentation_time;
                }
                continue;
            }

            let mut windows = self.layout.windows_for_output(output);
            let Some(mapped) = windows.find(|win| win.id().get() == id) else {
                continue;
            };

            let bbox = mapped
                .window
                .bbox_with_popups()
                .to_physical_precise_up(scale);

            match cast.ensure_size(bbox.size) {
                Ok(CastSizeChange::Ready) => (),
                Ok(CastSizeChange::Pending) => continue,
                Err(err) => {
                    warn!("error updating stream size, stopping screencast: {err:?}");
                    casts_to_stop.push(cast.session_id);
                }
            }

            if cast.check_time_and_schedule(output, target_presentation_time) {
                continue;
            }

            let mut elements = Vec::new();
            let mut pointer_location = Point::default();

            if self.pointer_visibility.is_visible() {
                if let Some((pointer_pos, win_pos)) = self.pointer_pos_for_window_cast(mapped) {
                    // Pointer location must be relative to the screencast buffer.
                    // - win_pos is the position of the main window surface in output-local
                    //   coordinates
                    // - bbox.loc moves us relative to the screencast buffer
                    let buf_pos = win_pos + bbox.loc.to_f64().to_logical(scale);
                    let output_pos = self.global_space.output_geometry(output).unwrap().loc;
                    pointer_location = pointer_pos - output_pos.to_f64() - buf_pos;

                    let pos = buf_pos.to_physical_precise_round(scale).upscale(-1);
                    self.render_pointer(renderer, output, &mut |elem| {
                        let elem =
                            RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                        elements.push(CastRenderElement::from(elem));
                    });
                }
            }

            let main_start = elements.len();
            mapped.render_for_screen_cast(renderer, scale, &mut |elem| {
                elements.push(CastRenderElement::from(elem))
            });

            let cursor_data = CursorData::compute(&elements, main_start, pointer_location, scale);

            match cast.dequeue_buffer_and_render(
                renderer,
                &elements,
                &cursor_data,
                bbox.size,
                scale,
            ) {
                Ok(true) => cast.last_frame_time = target_presentation_time,
                Ok(false) => (),
                Err(err) => {
                    warn!("error rendering cast, stopping screencast: {err:?}");
                    casts_to_stop.push(cast.session_id);
                }
            }
        }
        self.casting.casts = casts;

        for id in casts_to_stop {
            self.stop_cast(id);
        }
    }

    pub fn stop_cast(&mut self, session_id: CastSessionId) {
        let _span = tracy_client::span!("Niri::stop_cast");
        let _span = debug_span!("stop_cast", %session_id).entered();

        self.casting
            .pending_dynamic_casts
            .retain(|p| p.session_id != session_id);

        for i in (0..self.casting.casts.len()).rev() {
            let cast = &self.casting.casts[i];
            if cast.session_id != session_id {
                continue;
            }

            let cast = self.casting.casts.swap_remove(i);
            if let Err(err) = cast.stream.disconnect() {
                warn!("error disconnecting stream: {err:?}");
            }
        }

        self.casting.recompute_active_casts();

        let dbus = &self.dbus.as_ref().unwrap();
        let server = dbus.conn_screen_cast.as_ref().unwrap().object_server();
        let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{}", session_id.get());
        if let Ok(iface) = server.interface::<_, mutter_screen_cast::Session>(path) {
            let _span = tracy_client::span!("invoking Session::stop");

            async_io::block_on(async move {
                iface
                    .get()
                    .stop(server.inner(), iface.signal_emitter().clone())
                    .await
            });
        }
    }

    pub fn stop_casts_for_target(&mut self, target: CastTarget) {
        let _span = tracy_client::span!("Niri::stop_casts_for_target");

        // This is O(N^2) but it shouldn't be a problem I think.
        let mut saw_dynamic = false;
        let mut ids = Vec::new();
        for cast in &self.casting.casts {
            if cast.target != target {
                continue;
            }

            if cast.dynamic_target {
                saw_dynamic = true;
                continue;
            }

            ids.push(cast.session_id);
        }

        for id in ids {
            self.stop_cast(id);
        }

        // We don't stop dynamic casts, instead we switch them to Nothing.
        if saw_dynamic {
            self.event_loop
                .insert_idle(|state| state.set_dynamic_cast_target(CastTarget::Nothing));
        }
    }

    fn cast_params_for_window(&self, window_id: u64) -> Option<(Size<i32, Physical>, u32)> {
        let (_, mapped) = self
            .layout
            .windows()
            .find(|(_, m)| m.id().get() == window_id)?;
        let output = self.casting.mapped_cast_output.get(&mapped.window)?;
        let scale = Scale::from(output.current_scale().fractional_scale());
        let bbox = mapped
            .window
            .bbox_with_popups()
            .to_physical_precise_up(scale);
        let refresh = output.current_mode().unwrap().refresh as u32;
        Some((bbox.size, refresh))
    }
}

fn cast_params_for_output(output: &Output) -> (Size<i32, Physical>, u32) {
    let mode = output.current_mode().unwrap();
    let transform = output.current_transform();
    let size = transform.transform_size(mode.size);
    let refresh = mode.refresh as u32;
    (size, refresh)
}

/// Probes whether the primary renderer can bind an implicit-modifier (`Modifier::Invalid`) GBM
/// dmabuf as a render target.
///
/// Some drivers (e.g. NVIDIA) advertise the implicit modifier in `dmabuf_render_formats()`, but
/// fail to bind an implicit-modifier dmabuf for rendering. Any failure along the way is treated
/// conservatively as "not renderable".
fn probe_implicit_modifier_renderable(
    renderer: &mut GlesRenderer,
    gbm: &GbmDevice<DrmDeviceFd>,
) -> bool {
    let size = Size::from((64, 64));
    let mut dmabuf = match allocate_dmabuf(gbm, size, Fourcc::Argb8888, Modifier::Invalid) {
        Ok(dmabuf) => dmabuf,
        Err(_) => return false,
    };

    let renderable = renderer.bind(&mut dmabuf).is_ok();
    renderable
}

/// Decides the screencast format set to offer based on whether the implicit modifier is known to
/// be renderable.
///
/// `force_pipewire_invalid_modifier` is a debug flag that forces the implicit-modifier path for
/// testing purposes; when set, it always wins and the formats are returned unchanged.
fn strip_unsupported_implicit_modifier(
    formats: FormatSet,
    implicit_modifier_renderable: Option<bool>,
    force_pipewire_invalid_modifier: bool,
) -> FormatSet {
    if force_pipewire_invalid_modifier || implicit_modifier_renderable != Some(false) {
        return formats;
    }

    formats
        .into_iter()
        .filter(|f| f.modifier != Modifier::Invalid)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use smithay::backend::allocator::Format;

    use super::*;

    fn format(code: Fourcc, modifier: Modifier) -> Format {
        Format { code, modifier }
    }

    fn as_set(formats: &FormatSet) -> HashSet<Format> {
        formats.iter().copied().collect()
    }

    #[test]
    fn strip_removes_invalid_entries_when_probe_says_not_renderable() {
        let formats: FormatSet = [
            format(Fourcc::Argb8888, Modifier::Invalid),
            format(Fourcc::Argb8888, Modifier::Linear),
            format(Fourcc::Xrgb8888, Modifier::Invalid),
        ]
        .into_iter()
        .collect();

        let result = strip_unsupported_implicit_modifier(formats, Some(false), false);

        let expected: HashSet<Format> = [format(Fourcc::Argb8888, Modifier::Linear)]
            .into_iter()
            .collect();
        assert_eq!(as_set(&result), expected);
    }

    #[test]
    fn strip_is_identity_when_probe_says_renderable() {
        let formats: FormatSet = [
            format(Fourcc::Argb8888, Modifier::Invalid),
            format(Fourcc::Argb8888, Modifier::Linear),
        ]
        .into_iter()
        .collect();

        let result = strip_unsupported_implicit_modifier(formats.clone(), Some(true), false);

        assert_eq!(as_set(&result), as_set(&formats));
    }

    #[test]
    fn debug_flag_bypasses_strip_even_when_probe_says_not_renderable() {
        let formats: FormatSet = [format(Fourcc::Argb8888, Modifier::Invalid)]
            .into_iter()
            .collect();

        let result = strip_unsupported_implicit_modifier(formats.clone(), Some(false), true);

        assert_eq!(as_set(&result), as_set(&formats));
    }

    #[test]
    fn strip_yields_empty_set_when_all_entries_are_invalid() {
        let formats: FormatSet = [
            format(Fourcc::Argb8888, Modifier::Invalid),
            format(Fourcc::Xrgb8888, Modifier::Invalid),
        ]
        .into_iter()
        .collect();

        let result = strip_unsupported_implicit_modifier(formats, Some(false), false);

        assert_eq!(result.iter().count(), 0);
    }

    #[test]
    fn strip_is_identity_when_probe_has_not_run() {
        let formats: FormatSet = [format(Fourcc::Argb8888, Modifier::Invalid)]
            .into_iter()
            .collect();

        let result = strip_unsupported_implicit_modifier(formats.clone(), None, false);

        assert_eq!(as_set(&result), as_set(&formats));
    }
}

niri_render_elements! {
    CastRenderElement<R> => {
        Output = OutputRenderElements<R>,
        Window = WindowCastRenderElements<R>,
        Pointer = PointerRenderElements<R>,
        RelocatedPointer = RelocateRenderElement<PointerRenderElements<R>>,
    }
}

/// Match the Zoom client across Wayland-native app-id and xwayland WM_CLASS
/// (xwayland-satellite forwards `WM_CLASS` as the xdg-shell `app_id`).
///
/// Matched exactly, case-sensitively, against `"zoom"` (xwayland WM_CLASS on
/// Linux Zoom builds) or `"Zoom"`. Variants like `"Zoom Workplace"` or
/// `"ZOOM"` are NOT matched — this matches the epic-spec regex `^[Zz]oom$`.
/// Widen here if a future Zoom build ships under a different app-id.
pub fn is_zoom_app_id(app_id: &str) -> bool {
    matches!(app_id, "zoom" | "Zoom")
}

#[cfg(test)]
mod auto_hide_tests {
    use super::*;

    #[test]
    fn zoom_app_id_lowercase() {
        assert!(is_zoom_app_id("zoom"));
    }

    #[test]
    fn zoom_app_id_titlecase() {
        assert!(is_zoom_app_id("Zoom"));
    }

    #[test]
    fn zoom_app_id_rejects_others() {
        assert!(!is_zoom_app_id("zoomer"));
        assert!(!is_zoom_app_id("ZoomCorp"));
        assert!(!is_zoom_app_id("firefox"));
        assert!(!is_zoom_app_id(""));
        assert!(!is_zoom_app_id("ZOOM"));
    }
}
