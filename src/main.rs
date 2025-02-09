#![windows_subsystem = "windows"]
use eframe::egui;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use confy;
use egui::Id;
// For persistent configuration
use image; // For decoding PNG avatar images


/// Represents one replay item as returned by the API.
#[derive(Debug, Deserialize, Clone)]
struct Replay {
    _id: String,
    shack: bool,
    workshop_mods: String,
    workshop_id: String,
    competitive: bool,
    gameMode: String,
    created: String,
    live: bool,
    friendlyName: String,
    totalByes: u64,
    users: Vec<String>,
    secondsSince: u64,
    modcount: u64,
}

/// Settings that control the server address, refresh interval, auto‑refresh,
/// and auto‑download functionality. These settings are persisted using confy.
#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    server_addr: String,
    refresh_interval: u64, // seconds
    auto_refresh: bool,
    auto_download_filter: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_addr: "http://server:3000".to_owned(),
            refresh_interval: 1200,
            auto_refresh: false,
            auto_download_filter: String::new(),
        }
    }
}

/// Top‑level pages.
enum Page {
    Replays,
    Settings,
}

/// The result returned by a download thread.
#[derive(Clone)]
enum DownloadResult {
    Success(String),
    Failure(String),
}

/// Main application state.
struct MyApp {
    /// Latest replay list from the server.
    replays: Vec<Replay>,
    /// Receiver for updated replay lists.
    list_rx: mpsc::Receiver<Vec<Replay>>,
    /// Sender for updated replay lists (used for manual refresh).
    list_tx: mpsc::Sender<Vec<Replay>>,
    /// Shared settings (persisted via confy).
    settings: Arc<Mutex<Settings>>,
    /// Currently active page.
    current_page: Page,
    /// Manual filter for user id.
    filter_user: String,
    /// Manual filter for workshop mods.
    filter_workshop_mods: String,
    /// Manual filter for workshop id.
    filter_workshop_id: String,
    // Download state:
    /// True while waiting for a download API call to return.
    is_downloading: bool,
    /// When set, displays a popup notifying the download result.
    download_result: Option<DownloadResult>,
    /// Channel used to send download results from the download thread.
    download_tx: mpsc::Sender<DownloadResult>,
    download_rx: mpsc::Receiver<DownloadResult>,
    /// Keeps track of replay IDs that have been auto‑downloaded.
    downloaded_replays: HashSet<String>,
    /// --- Fields for loading user avatars ---
    /// A channel to receive (user, image) pairs after downloading avatars.
    profile_tx: mpsc::Sender<(String, egui::ColorImage)>,
    profile_rx: mpsc::Receiver<(String, egui::ColorImage)>,
    /// A cache mapping user id to a loaded texture.
    profile_textures: HashMap<String, egui::TextureHandle>,
    /// Track which user IDs are currently being loaded.
    loading_profiles: HashSet<String>,
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Load settings from disk using confy (or use defaults).
        let loaded_settings: Settings = confy::load("localpavtv_gui", None).unwrap_or_default();
        let settings = Arc::new(Mutex::new(loaded_settings));
        let settings_clone = settings.clone();

        // Create a channel for replay list updates.
        let (list_tx, list_rx) = mpsc::channel();
        // Clone the sender for use in the background thread.
        let list_tx_for_thread = list_tx.clone();

        // Create a channel for download events.
        let (download_tx, download_rx) = mpsc::channel();

        // Create a channel for profile image updates.
        let (profile_tx, profile_rx) = mpsc::channel();

        // Spawn a background thread to auto‑refresh the replay list (if enabled).
        thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            loop {
                let (server_addr, refresh_interval, auto_refresh) = {
                    let s = settings_clone.lock().unwrap();
                    (
                        s.server_addr.clone(),
                        s.refresh_interval,
                        s.auto_refresh,
                    )
                };
                if auto_refresh {
                    let list_url = format!("{}/list", server_addr);
                    match client.get(&list_url).send() {
                        Ok(response) => {
                            if let Ok(replays) = response.json::<Vec<Replay>>() {
                                if list_tx_for_thread.send(replays).is_err() {
                                    break;
                                }
                            } else {
                                eprintln!("Error parsing JSON from {}", list_url);
                            }
                        }
                        Err(err) => {
                            eprintln!("Error fetching {}: {}", list_url, err);
                        }
                    }
                }
                thread::sleep(Duration::from_secs(refresh_interval));
            }
        });

        Self {
            replays: Vec::new(),
            list_rx,
            list_tx, // used for manual refresh as well
            settings,
            current_page: Page::Replays,
            filter_user: String::new(),
            filter_workshop_mods: String::new(),
            filter_workshop_id: String::new(),
            is_downloading: false,
            download_result: None,
            download_tx,
            download_rx,
            downloaded_replays: HashSet::new(),
            profile_tx,
            profile_rx,
            profile_textures: HashMap::new(),
            loading_profiles: HashSet::new(),
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any loaded profile images received from background threads.
        while let Ok((user, color_image)) = self.profile_rx.try_recv() {
            let texture_handle = ctx.load_texture(
                &format!("avatar_{}", user),
                color_image,
                egui::TextureOptions {
                    magnification: egui::TextureFilter::Linear,
                    minification: egui::TextureFilter::Linear,
                    ..Default::default()
                },
            );

            self.profile_textures.insert(user.clone(), texture_handle);
            // Remove the user from the "loading" set.
            self.loading_profiles.remove(&user);
        }

        // If a download is in progress, check if it has completed.
        if self.is_downloading {
            if let Ok(result) = self.download_rx.try_recv() {
                self.is_downloading = false;
                self.download_result = Some(result);
            } else {
                // Show a loading overlay while downloading.
                egui::Area::new(Id::from("loading_overlay"))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        let rect = ctx.input(|i| i.screen_rect());
                        ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
                        ui.allocate_ui(rect.size(), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label("Downloading replay, please wait...");
                            });
                        });
                    });
                return;
            }
        }

        // If a download result is available, show a modal popup.
        if let Some(download_result) = self.download_result.clone() {
            let msg = match download_result {
                DownloadResult::Success(s) => s,
                DownloadResult::Failure(s) => s,
            };
            egui::Window::new("Download Complete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(&msg);
                    if ui.button("OK").clicked() {
                        self.download_result = None;
                    }
                });
        }

        // Process new replay lists (from auto‑refresh or manual refresh).
        while let Ok(new_replays) = self.list_rx.try_recv() {
            self.replays = new_replays;
        }

        // Top navigation menu.
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(matches!(self.current_page, Page::Replays), "Replays")
                    .clicked()
                {
                    self.current_page = Page::Replays;
                }
                if ui
                    .selectable_label(matches!(self.current_page, Page::Settings), "Settings")
                    .clicked()
                {
                    self.current_page = Page::Settings;
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.current_page {
            Page::Replays => {
                ui.heading("LocalPavTV_GUI");
                ui.separator();

                // Manual Refresh Button.
                if ui.button("Refresh").clicked() {
                    let server_addr = {
                        let s = self.settings.lock().unwrap();
                        s.server_addr.clone()
                    };
                    let list_tx = self.list_tx.clone();
                    thread::spawn(move || {
                        let client = reqwest::blocking::Client::new();
                        let list_url = format!("{}/list", server_addr);
                        if let Ok(response) = client.get(&list_url).send() {
                            if let Ok(replays) = response.json::<Vec<Replay>>() {
                                let _ = list_tx.send(replays);
                            }
                        }
                    });
                }
                ui.separator();

                // Filter fields.
                ui.horizontal(|ui| {
                    ui.label("Filter by user id:");
                    ui.text_edit_singleline(&mut self.filter_user);
                });
                ui.horizontal(|ui| {
                    ui.label("Filter by Workshop Mods:");
                    ui.text_edit_singleline(&mut self.filter_workshop_mods);
                });
                ui.horizontal(|ui| {
                    ui.label("Filter by Workshop ID:");
                    ui.text_edit_singleline(&mut self.filter_workshop_id);
                });
                ui.separator();

                // Sort replays (newest first: lowest secondsSince).
                let mut sorted_replays = self.replays.clone();
                sorted_replays.sort_by_key(|r| r.secondsSince);

                // Apply manual filters.
                let filtered_replays: Vec<Replay> = sorted_replays
                    .into_iter()
                    .filter(|r| {
                        let user_ok = self.filter_user.is_empty()
                            || r.users.iter().any(|user| user.contains(&self.filter_user));
                        let mods_ok = self.filter_workshop_mods.is_empty()
                            || r.workshop_mods.contains(&self.filter_workshop_mods);
                        let wid_ok = self.filter_workshop_id.is_empty()
                            || r.workshop_id.contains(&self.filter_workshop_id);
                        user_ok && mods_ok && wid_ok
                    })
                    .collect();

                // Display the replay list.
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    for replay in filtered_replays {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(format!("Friendly Name: {}", replay.friendlyName));
                                // Bigger, square download button.
                                if ui
                                    .add_sized(egui::vec2(160.0, 60.0), egui::Button::new("Download"))
                                    .clicked()
                                {
                                    self.is_downloading = true;
                                    // Mark replay as downloaded to avoid duplicate auto‑download.
                                    self.downloaded_replays.insert(replay._id.clone());
                                    let replay_id = replay._id.clone();
                                    let server_addr = {
                                        let s = self.settings.lock().unwrap();
                                        s.server_addr.clone()
                                    };
                                    let download_tx = self.download_tx.clone();
                                    thread::spawn(move || {
                                        // Build a client with no timeout.
                                        let client = reqwest::blocking::Client::builder()
                                            .timeout(None)
                                            .build()
                                            .expect("Failed to build client");
                                        let download_url =
                                            format!("{}/download/{}", server_addr, replay_id);
                                        match client.get(&download_url).send() {
                                            Ok(resp) => {
                                                if resp.status().is_success() {
                                                    let _ = download_tx.send(DownloadResult::Success(
                                                        format!("Downloaded replay {}", replay_id),
                                                    ));
                                                } else {
                                                    let _ = download_tx.send(DownloadResult::Failure(
                                                        format!(
                                                            "Failed to download replay {}: HTTP {}",
                                                            replay_id,
                                                            resp.status()
                                                        ),
                                                    ));
                                                }
                                            }
                                            Err(err) => {
                                                let _ = download_tx.send(DownloadResult::Failure(
                                                    format!("Error downloading {}: {}", replay_id, err),
                                                ));
                                            }
                                        }
                                    });
                                }
                            });
                            // Instead of listing user IDs as text, display their avatar images.
                            ui.horizontal(|ui| {
                                for user in &replay.users {
                                    // If we already loaded the avatar, show it.
                                    if let Some(texture) = self.profile_textures.get(user) {
                                        if ui
                                            .add_sized(egui::vec2(64.0, 64.0), egui::ImageButton::new(texture))
                                            .clicked()
                                        {
                                            ctx.output_mut(|output| {
                                                output.copied_text = user.clone();
                                            });

                                        }

                                    } else {
                                        // Otherwise, show a placeholder button.
                                        if ui.add_sized([64.0, 64.0], egui::Button::new("Loading")).clicked() {
                                            ctx.output_mut(|output| {
                                                output.copied_text = user.clone();
                                            });

                                        }
                                        // If not already loading, spawn a thread to fetch the avatar.
                                        if !self.loading_profiles.contains(user) {
                                            self.loading_profiles.insert(user.clone());
                                            let user_clone = user.clone();
                                            let profile_tx = self.profile_tx.clone();
                                            thread::spawn(move || {
                                                let client = reqwest::blocking::Client::builder()
                                                    .timeout(None)
                                                    .build()
                                                    .expect("Failed to build client");
                                                let url = format!("http://prod.cdn.pavlov-vr.com/avatar/{}.png", user_clone);
                                                match client.get(&url).send() {
                                                    Ok(resp) => {
                                                        if let Ok(bytes) = resp.bytes() {
                                                            if let Ok(img) = image::load_from_memory(&bytes) {
                                                                let img = img.to_rgba8();
                                                                let size = [img.width() as usize, img.height() as usize];
                                                                let pixels = img.into_raw();
                                                                let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                                                                let _ = profile_tx.send((user_clone, color_image));
                                                            }
                                                        }
                                                    }
                                                    Err(err) => {
                                                        eprintln!("Error loading avatar for {}: {}", user_clone, err);
                                                    }
                                                }
                                            });
                                        }
                                    }
                                }
                            });
                            ui.label(format!("Workshop Mods: {}", replay.workshop_mods));
                            ui.label(format!("Game Mode: {}", replay.gameMode));
                            ui.label(format!("Mod Count: {}", replay.modcount));
                            ui.label(format!("Seconds Since: {}", replay.secondsSince));
                            ui.label(format!("Workshop ID: {}", replay.workshop_id));
                        });
                        ui.add_space(10.0);
                    }
                });

                // Auto‑download: If an auto‑download filter is set, trigger auto‑download for matching replays.
                if !self.is_downloading {
                    let auto_filter = {
                        let s = self.settings.lock().unwrap();
                        s.auto_download_filter.clone()
                    };
                    if !auto_filter.is_empty() {
                        for replay in &self.replays {
                            if !self.downloaded_replays.contains(&replay._id)
                                && (replay.users.iter().any(|user| user.contains(&auto_filter))
                                || replay.workshop_mods.contains(&auto_filter)
                                || replay.workshop_id.contains(&auto_filter))
                            {
                                self.is_downloading = true;
                                self.downloaded_replays.insert(replay._id.clone());
                                let replay_id = replay._id.clone();
                                let server_addr = {
                                    let s = self.settings.lock().unwrap();
                                    s.server_addr.clone()
                                };
                                let download_tx = self.download_tx.clone();
                                thread::spawn(move || {
                                    let client = reqwest::blocking::Client::builder()
                                        .timeout(None)
                                        .build()
                                        .expect("Failed to build client");
                                    let download_url =
                                        format!("{}/download/{}", server_addr, replay_id);
                                    match client.get(&download_url).send() {
                                        Ok(resp) => {
                                            if resp.status().is_success() {
                                                let _ = download_tx.send(DownloadResult::Success(
                                                    format!("Auto-downloaded replay {}", replay_id),
                                                ));
                                            } else {
                                                let _ = download_tx.send(DownloadResult::Failure(
                                                    format!(
                                                        "Failed auto-download of replay {}: HTTP {}",
                                                        replay_id,
                                                        resp.status()
                                                    ),
                                                ));
                                            }
                                        }
                                        Err(err) => {
                                            let _ = download_tx.send(DownloadResult::Failure(
                                                format!("Error auto-downloading {}: {}", replay_id, err),
                                            ));
                                        }
                                    }
                                });
                                break; // Only trigger one auto-download at a time.
                            }
                        }
                    }
                }
            }
            Page::Settings => {
                ui.heading("Settings");
                ui.separator();
                if let Ok(mut settings) = self.settings.lock() {
                    ui.label("Server Address:");
                    ui.text_edit_singleline(&mut settings.server_addr);
                    ui.add_space(10.0);
                    ui.label("Refresh Interval (seconds):");
                    ui.add(
                        egui::Slider::new(&mut settings.refresh_interval, 1..=86400)
                            .text("seconds"),
                    );
                    ui.add_space(10.0);
                    // Toggle auto‑refresh.
                    if settings.auto_refresh {
                        if ui.button("Stop Auto Refresh").clicked() {
                            settings.auto_refresh = false;
                        }
                    } else {
                        if ui.button("Start Auto Refresh").clicked() {
                            settings.auto_refresh = true;
                        }
                    }
                    ui.add_space(10.0);
                    ui.label("Auto Download Filter (download replay if matched):");
                    ui.text_edit_singleline(&mut settings.auto_download_filter);
                    ui.add_space(10.0);
                    // Offload saving settings to a background thread to avoid UI freeze.
                    if ui.button("Save Settings").clicked() {
                        let settings_clone = settings.clone();
                        thread::spawn(move || {
                            match confy::store("localpavtv_gui", None, &settings_clone) {
                                Ok(_) => println!("Settings saved."),
                                Err(err) => eprintln!("Error saving settings: {:?}", err),
                            }
                        });
                    }
                } else {
                    ui.label("Error accessing settings");
                }
            }
        });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "LocalPavTV_GUI",
        options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc)))),
    )
}
