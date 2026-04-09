use mpd_client::responses::Song;
use reqwest::multipart;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use lofty::prelude::*;
use lofty::probe::Probe;

pub struct AlbumArtClient {
    cache: HashMap<String, String>,
    http_client: reqwest::Client,
    music_base_path: PathBuf,
}

impl AlbumArtClient {
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            cache: HashMap::new(),
            http_client,
            music_base_path: PathBuf::from("/home/vecondite/music"),
        }
    }

    pub async fn get_album_art_url(&mut self, song: &Song, _client: &mpd_client::Client) -> Option<String> {
        let uri = &song.url;

        if let Some(url) = self.cache.get(uri) {
            return Some(url.clone());
        }

        let full_path = self.music_base_path.join(uri);
        println!("[ART] Checking local file: {:?}", full_path);

        let mut art_bytes = None;

        if let Ok(tagged_file) = Probe::open(&full_path).and_then(|p| p.read()) {
            for tag in tagged_file.tags() {
                if let Some(picture) = tag.pictures().first() {
                    // Fixed: changed {} to {:?} for tag.tag_type()
                    println!("[ART] Found picture in {:?} tags ({} bytes)", tag.tag_type(), picture.data().len());
                    art_bytes = Some(picture.data().to_vec());
                    break;
                }
            }
        }

        if let Some(bytes) = art_bytes {
            println!("[ART] Uploading extracted art to Litterbox (24h expiry)...");
            match self.upload_to_litterbox(bytes).await {
                Ok(url) => {
                    println!("[ART] Success: {}", url);
                    self.cache.insert(uri.clone(), url.clone());
                    Some(url)
                }
                Err(e) => {
                    println!("[ART] Litterbox upload failed: {}", e);
                    if let Some(source) = std::error::Error::source(&*e) {
                         println!("[ART] Error Source: {}", source);
                    }
                    None
                }
            }
        } else {
            println!("[ART] No embedded art found in: {:?}", full_path);
            None
        }
    }

    async fn upload_to_litterbox(&self, bytes: Vec<u8>) -> Result<String, Box<dyn std::error::Error>> {
        let form = multipart::Form::new()
            .text("reqtype", "fileupload")
            .text("time", "24h")
            .part("fileToUpload", multipart::Part::bytes(bytes).file_name("cover.jpg"));

        let response = self.http_client
            .post("https://litterbox.catbox.moe/resources/internals/api.php")
            .multipart(form)
            .send()
            .await?
            .text()
            .await?;

        if response.starts_with("https://") {
            Ok(response.trim().to_string())
        } else {
            Err(format!("Litterbox error: {}", response).into())
        }
    }
}
