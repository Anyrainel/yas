mod game_info;
mod os;
mod game_info_builder;
mod ui;
mod resolution_family;

pub use game_info_builder::GameInfoBuilder;
pub use ui::{UI, Platform};
pub use resolution_family::is_16x9;
pub use game_info::GameInfo;
