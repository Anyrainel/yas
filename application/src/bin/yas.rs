use yas::utils::press_any_key_to_continue;
use yas_genshin::cli::GoodScannerApplication;

fn init_cli() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Install a custom panic hook so that panics (from unwrap, expect, panic!, etc.)
    // print the error and wait for user input before the process exits.
    // Without this, the console window closes immediately and users can't see the error.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        press_any_key_to_continue();
    }));
}

pub fn main() {
    // No CLI args → launch GUI; any args → CLI mode
    if std::env::args().len() == 1 {
        yas_application::gui::run_gui();
        return;
    }

    // CLI mode: existing behavior
    init_cli();
    let command = GoodScannerApplication::build_command();
    let matches = match command.try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", e);
            press_any_key_to_continue();
            std::process::exit(if e.use_stderr() { 1 } else { 0 });
        }
    };

    let application = GoodScannerApplication::new(matches);
    match application.run() {
        Ok(_) => {
            press_any_key_to_continue();
        },
        Err(e) => {
            log::error!("错误 / Error: {}", e);
            press_any_key_to_continue();
        },
    }
}
