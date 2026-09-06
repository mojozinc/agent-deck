pub mod mock;
pub mod native_windows;
pub mod wsl2_bridge;

use agent_deck_core::SessionEvent;
use std::sync::mpsc::Sender;

pub trait StreamAdapter: Send + 'static {
    fn start(&mut self, tx: Sender<SessionEvent>);
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

