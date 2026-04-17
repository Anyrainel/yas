use crate::game_info::UI;
use crate::game_info::ui::Platform;
use crate::positioning::Rect;

#[derive(Clone, Debug)]
pub struct GameInfo {
    pub window: Rect<i32>,
    pub is_cloud: bool,
    pub ui: UI,
    pub platform: Platform,
}
