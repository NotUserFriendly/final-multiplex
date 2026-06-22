mod bridge;
mod ui;

fn boot() -> ui::App {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "scene.toml".to_string());
    ui::App::init(std::path::Path::new(&config_path))
}

fn main() -> iced::Result {
    iced::application(boot, ui::App::update, ui::App::view)
        .title("Final Multiplex")
        .subscription(ui::App::subscription)
        .run()
}
