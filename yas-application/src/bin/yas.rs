use yas::utils::press_any_key_to_continue;
use yas_genshin::application::GoodScannerApplication;

fn init() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();
}

pub fn main() {
    init();
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
            log::error!("error: {}", e);
            press_any_key_to_continue();
        },
    }
}
