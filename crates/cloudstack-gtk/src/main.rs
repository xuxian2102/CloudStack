mod i18n;
mod preview;
mod search;
mod tasks;
mod window;

use adw::prelude::*;

pub(crate) const APPLICATION_ID: &str = "dev.xuxian.cloudstack";
pub(crate) const LEGACY_APPLICATION_ID: &str = "dev.xuxian.blogeditor";

fn main() -> gtk::glib::ExitCode {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("CloudStack 只支持原生 Wayland；当前会话没有 WAYLAND_DISPLAY。");
        return gtk::glib::ExitCode::FAILURE;
    }
    // 应用没有 X11 运行契约；即使系统 GTK 同时编译了 X11 backend，也不允许回退。
    std::env::set_var("GDK_BACKEND", "wayland");
    i18n::initialize();

    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    application.connect_activate(window::present);
    application.run()
}
