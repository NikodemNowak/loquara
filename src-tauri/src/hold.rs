use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Default)]
pub(crate) struct HoldGesture {
    released: AtomicBool,
    cancelled: AtomicBool,
    main_released: AtomicBool,
}
impl HoldGesture {
    pub fn released(&self) -> bool { self.released.load(Ordering::SeqCst) }
    pub fn main_released(&self) -> bool { self.main_released.load(Ordering::SeqCst) }
    pub fn cancelled(&self) -> bool { self.cancelled.load(Ordering::SeqCst) }
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::SeqCst); self.released.store(true, Ordering::SeqCst); }
}

#[derive(Default)]
pub(crate) struct HoldController {
    current: Option<Arc<HoldGesture>>,
    pub suspended: bool,
}
impl HoldController {
    pub fn press(&mut self, idle: bool) -> Option<Arc<HoldGesture>> {
        if self.suspended || !idle || self.current.is_some() { return None; }
        let gesture = Arc::new(HoldGesture::default());
        self.current = Some(gesture.clone());
        Some(gesture)
    }
    pub fn sample(&mut self, gesture: &Arc<HoldGesture>, chord_down: bool, main_down: bool, escape: bool) {
        if !self.current.as_ref().is_some_and(|current| Arc::ptr_eq(current, gesture)) { return; }
        if escape { gesture.cancel(); }
        if !chord_down { gesture.released.store(true, Ordering::SeqCst); }
        if !main_down { gesture.main_released.store(true, Ordering::SeqCst); self.current = None; }
    }
    pub fn abandon(&mut self, gesture: &Arc<HoldGesture>) {
        gesture.cancel();
        if self.current.as_ref().is_some_and(|current| Arc::ptr_eq(current, gesture)) { self.current = None; }
    }
    pub fn current(&self) -> Option<Arc<HoldGesture>> { self.current.clone() }
    pub fn invalidate(&mut self) { if let Some(gesture) = &self.current { gesture.cancel(); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hold_release_during_start_is_retained_and_repeat_does_not_restart() {
        let mut control = HoldController::default();
        let hold = control.press(true).expect("first hold starts");
        assert!(control.press(true).is_none());
        control.sample(&hold, false, true, false);
        assert!(hold.released());
        assert!(!hold.cancelled());
        assert!(control.press(true).is_none(), "modifier re-press while main held is not a new take");
        control.sample(&hold, false, false, false);
        let next = control.press(true).unwrap();
        control.sample(&hold, false, false, true);
        assert!(!next.released(), "stale monitor cannot affect new gesture");
    }
    #[test]
    fn hold_cancel_suspension_and_unowned_recording_never_start() {
        let mut control = HoldController::default();
        assert!(control.press(false).is_none());
        control.suspended = true;
        assert!(control.press(true).is_none());
        control.suspended = false;
        let hold = control.press(true).unwrap();
        control.sample(&hold, true, true, true);
        assert!(hold.cancelled());
        assert!(control.press(true).is_none());
        control.sample(&hold, false, false, false);
        let next = control.press(true).unwrap();
        control.invalidate();
        assert!(next.cancelled());
    }
}
