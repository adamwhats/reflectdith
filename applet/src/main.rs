mod app;
fn main() -> cosmic::iced::Result {
    if std::env::args().any(|arg| arg == "--settings") {
        cosmic::app::run::<app::Applet>(cosmic::app::Settings::default(), true)
    } else {
        cosmic::applet::run::<app::Applet>(false)
    }
}
