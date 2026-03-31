/// Capture monitor: orchestrates packet capture, decryption, and data accumulation.
///
/// Ported from irminsul's `monitor.rs`, simplified for yas integration.
/// The monitor runs on a tokio runtime and communicates via channels.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use auto_artifactarium::{
    GamePacket, GameSniffer, matches_avatar_packet, matches_item_packet,
};
use base64::prelude::*;
use log::{error, info};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::data_cache::load_data_cache;
use super::data_types::DataCache;
use super::packet_capture::PacketCapture;
use super::player_data::{CaptureExportSettings, PlayerData};
use crate::scanner::common::models::GoodExport;

/// Commands the UI can send to the monitor.
pub enum CaptureCommand {
    StartCapture,
    StopCapture,
    Export {
        settings: CaptureExportSettings,
        reply: tokio::sync::oneshot::Sender<Result<GoodExport>>,
    },
}

/// State shared between the monitor and UI.
#[derive(Clone, Debug)]
pub struct CaptureState {
    pub capturing: bool,
    /// Both characters and items have been received; capture auto-stopped.
    pub complete: bool,
    pub has_characters: bool,
    pub has_items: bool,
    pub character_count: usize,
    pub weapon_count: usize,
    pub artifact_count: usize,
    pub error: Option<String>,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            capturing: false,
            complete: false,
            has_characters: false,
            has_items: false,
            character_count: 0,
            weapon_count: 0,
            artifact_count: 0,
            error: None,
        }
    }
}

/// The capture monitor. Runs on a tokio runtime.
pub struct CaptureMonitor {
    player_data: PlayerData,
    sniffer: GameSniffer,
    state: Arc<Mutex<CaptureState>>,
    capture_cancel_token: Option<CancellationToken>,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    packet_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl CaptureMonitor {
    /// Initialize the monitor: load data cache, set up sniffer.
    pub fn new(state: Arc<Mutex<CaptureState>>) -> Result<Self> {
        let data_cache = load_data_cache()?;
        let player_data = PlayerData::new(data_cache);
        let keys = load_keys()?;
        let sniffer = GameSniffer::new().set_initial_keys(keys);
        let (packet_tx, packet_rx) = mpsc::unbounded_channel();

        Ok(Self {
            player_data,
            sniffer,
            state,
            capture_cancel_token: None,
            packet_tx,
            packet_rx,
        })
    }

    /// Initialize with a pre-loaded DataCache (for testing or custom sources).
    pub fn new_with_data(data_cache: DataCache, state: Arc<Mutex<CaptureState>>) -> Result<Self> {
        let player_data = PlayerData::new(data_cache);
        let keys = load_keys()?;
        let sniffer = GameSniffer::new().set_initial_keys(keys);
        let (packet_tx, packet_rx) = mpsc::unbounded_channel();

        Ok(Self {
            player_data,
            sniffer,
            state,
            capture_cancel_token: None,
            packet_tx,
            packet_rx,
        })
    }

    /// Main event loop. Processes packets and UI commands.
    pub async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<CaptureCommand>) {
        loop {
            tokio::select! {
                Some(packet) = self.packet_rx.recv() => {
                    self.handle_packet(packet);
                }
                Some(cmd) = cmd_rx.recv() => {
                    if self.handle_command(cmd) {
                        break;
                    }
                }
                else => break,
            }
        }
    }

    /// Returns true if the loop should exit.
    fn handle_command(&mut self, cmd: CaptureCommand) -> bool {
        match cmd {
            CaptureCommand::StartCapture => {
                if self.capture_cancel_token.is_some() {
                    return false;
                }
                let cancel_token = CancellationToken::new();
                tokio::spawn(capture_task(cancel_token.clone(), self.packet_tx.clone()));
                self.capture_cancel_token = Some(cancel_token);
                if let Ok(mut state) = self.state.lock() {
                    state.capturing = true;
                    state.complete = false;
                    state.error = None;
                }
            }
            CaptureCommand::StopCapture => {
                self.stop_capture();
            }
            CaptureCommand::Export { settings, reply } => {
                let result = self.player_data.export(&settings);
                let _ = reply.send(result);
            }
        }
        false
    }

    fn stop_capture(&mut self) {
        if let Some(token) = self.capture_cancel_token.take() {
            token.cancel();
        }
        if let Ok(mut state) = self.state.lock() {
            state.capturing = false;
        }
    }

    fn handle_packet(&mut self, packet: Vec<u8>) {
        let Some(GamePacket::Commands(commands)) = self.sniffer.receive_packet(packet) else {
            return;
        };

        for command in commands {
            if let Some(items) = matches_item_packet(&command) {
                info!(
                    "捕获到物品数据包，共 {} 个物品 / Captured item packet with {} items",
                    items.len(),
                    items.len()
                );
                self.player_data.process_items(&items);
                if let Ok(mut state) = self.state.lock() {
                    state.has_items = true;
                    state.weapon_count = self.player_data.weapon_count();
                    state.artifact_count = self.player_data.artifact_count();
                }
            } else if let Some(avatars) = matches_avatar_packet(&command) {
                info!(
                    "捕获到角色数据包，共 {} 个角色 / Captured avatar packet with {} avatars",
                    avatars.len(),
                    avatars.len()
                );
                self.player_data.process_characters(&avatars);
                if let Ok(mut state) = self.state.lock() {
                    state.has_characters = true;
                    state.character_count = self.player_data.character_count();
                }
            }
        }

        // Auto-stop when we have both characters and items
        let should_stop = self
            .state
            .lock()
            .map_or(false, |s| s.has_characters && s.has_items && s.capturing);
        if should_stop {
            info!(
                "已收集到所有数据，自动停止抓包 / All data collected, stopping capture automatically"
            );
            self.stop_capture();
            if let Ok(mut state) = self.state.lock() {
                state.complete = true;
            }
        }
    }
}

async fn capture_task(
    cancel_token: CancellationToken,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> Result<()> {
    let mut capture =
        PacketCapture::new().map_err(|e| anyhow!("创建抓包失败 / Error creating packet capture: {e}"))?;
    info!("开始抓包 / Starting packet capture");
    loop {
        let packet = tokio::select!(
            packet = capture.next_packet() => packet,
            _ = cancel_token.cancelled() => break,
        );
        let packet = match packet {
            Ok(packet) => packet,
            Err(e) => {
                error!("接收数据包出错 / Error receiving packet: {e}");
                continue;
            }
        };
        if let Err(e) = packet_tx.send(packet) {
            error!("发送数据包出错 / Error sending captured packet: {e}");
        }
    }
    info!("抓包已停止 / Packet capture stopped");
    Ok(())
}

fn load_keys() -> Result<HashMap<u16, Vec<u8>>> {
    let keys: HashMap<u16, String> =
        serde_json::from_slice(include_bytes!("../../keys/gi.json"))?;

    keys.iter()
        .map(|(key, value)| -> Result<_, _> { Ok((*key, BASE64_STANDARD.decode(value)?)) })
        .collect::<Result<HashMap<_, _>>>()
}
