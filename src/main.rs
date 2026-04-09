use std::time::Duration;
use std::sync::Arc;
use tokio::sync::Mutex;
use discord_presence::models::ActivityType;
use discord_presence::Client as DiscordClient;
use mpd_client::client::ConnectionEvent::SubsystemChange;
use mpd_client::client::Subsystem;
use mpd_client::commands;
use mpd_client::responses::{PlayState, SongInQueue, Status};
use mpd_utils::MultiHostClient;
use regex::Regex;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::album_art::AlbumArtClient;
use crate::config::DisplayType as ConfigDisplayType;
use crate::mpd_conn::get_timestamp;
use config::Config;

mod album_art;
mod config;
mod mpd_conn;

pub const IDLE_TIME: u64 = 3;

fn map_display_type(display_type: ConfigDisplayType) -> discord_presence::models::DisplayType {
    match display_type {
        ConfigDisplayType::Name => discord_presence::models::DisplayType::Name,
        ConfigDisplayType::State => discord_presence::models::DisplayType::State,
        ConfigDisplayType::Details => discord_presence::models::DisplayType::Details,
    }
}

struct Tokens {
    details: Vec<String>,
    state: Vec<String>,
}

#[tokio::main]
async fn main() {
    let re = Regex::new(r"\$(\w+)").expect("Failed to parse regex");
    let config = Config::load();

    let tokens = Tokens {
        details: get_tokens(&re, &config.format.details),
        state: get_tokens(&re, &config.format.state),
    };

    let mut mpd = MultiHostClient::new(config.hosts.clone(), Duration::from_secs(IDLE_TIME));
    mpd.init();

    let (tx, mut rx) = mpsc::channel(16);
    let service = Arc::new(Mutex::new(Service::new(&config, tokens, tx)));
    
    {
        let mut s = service.lock().await;
        s.start();
    }

    println!("[RPC] Service Started. Monitoring MPD...");

    loop {
        tokio::select! {
            Ok(event) = mpd.recv() => {
                if matches!(*event, SubsystemChange(Subsystem::Player | Subsystem::Queue)) {
                    let s_clone = Arc::clone(&service);
                    let _ = mpd.with_client(|client| async move {
                        let status = client.command(commands::Status).await.ok();
                        let current_song = if status.is_some() {
                            client.command(commands::CurrentSong).await.ok().flatten()
                        } else {
                            None
                        };
                        if let Some(st) = status {
                            let mut s = s_clone.lock().await;
                            s.update_state(&st, current_song, &client).await;
                        }
                    }).await;
                }
            }
            Some(event) = rx.recv() => {
                match event {
                    ServiceEvent::Ready => println!("[RPC] Discord Gateway Ready."),
                    ServiceEvent::Error(err) => {
                        println!("[RPC] Error: {}", err);
                        sleep(Duration::from_secs(IDLE_TIME)).await;
                        let mut s = service.lock().await;
                        s.start();
                    }
                }
            },
        }
    }
}

enum ServiceEvent { Ready, Error(String) }

struct Service<'a> {
    config: &'a Config,
    album_art_client: AlbumArtClient,
    drpc: DiscordClient,
    tokens: Tokens,
}

impl<'a> Service<'a> {
    fn new(config: &'a Config, tokens: Tokens, event_tx: mpsc::Sender<ServiceEvent>) -> Self {
        let drpc = DiscordClient::with_error_config(config.id, Duration::from_secs(IDLE_TIME), Some(0));
        let tx = event_tx.clone();
        drpc.on_ready(move |_| { let _ = tx.try_send(ServiceEvent::Ready); }).persist();
        
        Self { config, album_art_client: AlbumArtClient::new(), drpc, tokens }
    }

    fn start(&mut self) { self.drpc.start(); }

    async fn update_state(&mut self, status: &Status, current_song: Option<SongInQueue>, client: &mpd_client::Client) {
        const MAX_BYTES: usize = 128;
        if matches!(status.state, PlayState::Playing) {
            if let Some(song_in_queue) = current_song {
                let song = song_in_queue.song;
                let mut details = clamp(replace_tokens(&self.config.format.details, &self.tokens.details, &song, status), MAX_BYTES);
                let state = clamp(replace_tokens(&self.config.format.state, &self.tokens.state, &song, status), MAX_BYTES);
                
                while details.chars().count() < 2 { details.push('\u{200B}'); }

                let url = self.album_art_client.get_album_art_url(&song, client).await;
                let timestamps = get_timestamp(status, self.config.format.timestamp);

                if let Some(ref u) = url {
                    println!("[RPC] Updating Presence with Art: {}", u);
                } else {
                    println!("[RPC] Updating Presence (No Art Found). Fallback: {}", self.config.format.large_image);
                }

                let _ = self.drpc.set_activity(|act| {
                    act.state(state).details(details).activity_type(ActivityType::Listening)
                        .status_display(map_display_type(self.config.format.display_type))
                        .assets(|mut assets| {
                            if let Some(u) = url { assets = assets.large_image(u); }
                            else if !self.config.format.large_image.is_empty() {
                                assets = assets.large_image(&self.config.format.large_image);
                            }
                            assets
                        }).timestamps(|_| timestamps)
                });
            }
        } else { 
            println!("[RPC] Player Paused/Stopped. Clearing activity.");
            let _ = self.drpc.clear_activity(); 
        }
    }
}

fn get_tokens(re: &Regex, format_string: &str) -> Vec<String> {
    re.captures_iter(format_string).map(|caps| caps[1].to_string()).collect()
}

fn replace_tokens(format_string: &str, tokens: &Vec<String>, song: &mpd_client::responses::Song, status: &Status) -> String {
    let mut res = format_string.to_string();
    for t in tokens {
        let val = mpd_conn::get_token_value(song, status, t);
        res = res.replace(&format!("${t}"), &val);
    }
    res
}

fn clamp(mut str: String, len: usize) -> String {
    if str.len() > len {
        str.truncate(len - 3);
        str.push_str("...");
    }
    str
}
