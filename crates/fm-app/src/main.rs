mod bridge;
mod ui;

fn main() -> iced::Result {
    iced::application(ui::App::default, ui::App::update, ui::App::view)
        .title("Final Multiplex")
        .run()
}
