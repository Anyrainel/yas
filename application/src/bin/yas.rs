// Hide console window in GUI mode. CLI mode reattaches below.
#![windows_subsystem = "windows"]

use yas::utils::press_any_key_to_continue;
use yas_genshin::cli::GoodScannerApplication;

/// Attach to the parent process's console (e.g. cmd.exe, PowerShell).
/// If no parent console exists, allocate a new one.
/// This is needed because `windows_subsystem = "windows"` detaches from the console.
#[cfg(windows)]
fn attach_console() {
    use std::os::raw::c_int;
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> c_int;
        fn AllocConsole() -> c_int;
    }
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

fn init_cli() {
    #[cfg(windows)]
    attach_console();

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

    // CLI mode: attach console and run
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
