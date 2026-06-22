use iced::{widget::text, Element, Task};

/// Top-level application state.
#[derive(Default)]
pub struct App;

#[derive(Debug, Clone)]
pub enum Message {}

impl App {
    pub fn update(&mut self, _message: Message) -> Task<Message> {
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        // Phase 1 will replace this with the tile grid + transport controls.
        text("Final Multiplex — scaffold").into()
    }
}
