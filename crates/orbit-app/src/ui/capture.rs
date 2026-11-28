//! The installed package checks the actual window; staged scenes are opt-in.
use super::*;

impl App {
    pub(super) fn start_capture(&mut self, id: window::Id) -> Task<Message> {
        if !matches!(self.launch, crate::Launch::Normal) && self.harness.is_none() {
            eprintln!(
                "Window capture failed to initialize isolated storage: {:?}",
                self.error
            );
            return iced::exit();
        }
        #[cfg(any(test, feature = "staged-capture"))]
        {
            self.start_staged_capture(id)
        }
        #[cfg(not(any(test, feature = "staged-capture")))]
        {
            match self.launch {
                crate::Launch::Normal => Task::none(),
                crate::Launch::Smoke => window::screenshot(id).map(Message::Screenshot),
            }
        }
    }

    pub(super) fn smoke_frame(&mut self, screenshot: window::Screenshot) -> Task<Message> {
        let pixels = &screenshot.rgba;
        let valid = screenshot.size.width > 0
            && screenshot.size.height > 0
            && pixels.len() == screenshot.size.width as usize * screenshot.size.height as usize * 4
            && pixels.as_chunks::<4>().0.iter().any(|p| p != &pixels[..4]);
        if !valid {
            eprintln!("Window capture returned an empty or uniform frame.");
            return iced::exit();
        }
        #[cfg(any(test, feature = "staged-capture"))]
        {
            self.staged_frame(screenshot)
        }
        #[cfg(not(any(test, feature = "staged-capture")))]
        {
            println!("SMOKE_OK: captured a nonuniform native Wayland frame");
            self.smoke_passed.store(true, Ordering::Relaxed);
            iced::exit()
        }
    }
}
