use crate::Odysseus;
use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
        GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData,
        MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
};

pub struct MoveSurfaceGrab {
    pub start_data: PointerGrabStartData<Odysseus>,
    pub window: Window,
    pub initial_window_location: Point<i32, Logical>,
}

/// Mouvement minimal avant de décrocher une fenêtre rangée (un simple clic ne la bouge pas)
const UNSNAP_DISTANCE: f64 = 6.0;

impl PointerGrab<Odysseus> for MoveSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        // Fenêtre rangée (moitié, quart, agrandie) : elle reprend sa taille d'avant
        if (delta.x.abs() > UNSNAP_DISTANCE || delta.y.abs() > UNSNAP_DISTANCE)
            && let Some(loc) = data.unsnap_for_move(&self.window, event.location)
        {
            self.initial_window_location = (loc.to_f64() - delta).to_i32_round();
        }
        let new_location = self.initial_window_location.to_f64() + delta;
        data.space
            .map_element(self.window.clone(), new_location.to_i32_round(), true);
        data.update_snap_preview(&self.window, event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The button is a button code as defined in the
        // Linux kernel's linux/input-event-codes.h header file, e.g. BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;

        if !handle.current_pressed().contains(&BTN_LEFT) {
            // Relâché contre un bord : la fenêtre se range dans la zone proposée
            let pos = handle.current_location();
            data.finish_snap(&self.window, pos);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut Odysseus, handle: &mut PointerInnerHandle<'_, Odysseus>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut Odysseus,
        handle: &mut PointerInnerHandle<'_, Odysseus>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<Odysseus> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut Odysseus) {
        data.snap_preview = None;
    }
}
