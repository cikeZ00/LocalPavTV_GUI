use eframe::egui;
use reqwest;
use serde::Deserialize;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

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

/// Application settings used for the server address and refresh interval.
#[derive(Clone)]
struct Settings {
    server_addr: String,
    refresh_interval: u64, // in seconds
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_addr: "http://192.168.0.13:300".to_owned(),
            refresh_interval: 10,
        }
    }
}

/// Represents the currently visible page in the UI.
enum Page {
    Replays,
    Settings,
}

/// Main application state.
struct MyApp {
    /// The list of replays as fetched from the server.
    replays: Vec<Replay>,
    /// Receiver for new replay lists coming from the background thread.
    list_rx: mpsc::Receiver<Vec<Replay>>,
    /// Shared settings (server address and refresh interval) used by both UI and background thread.
    settings: Arc<Mutex<Settings>>,
    /// The current page shown in the UI.
    current_page: Page,
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Create our shared settings.
        let settings = Arc::new(Mutex::new(Settings::default()));
        let settings_clone = settings.clone();

        // Create a channel so that the background thread can send updated replay lists.
        let (tx, rx) = mpsc::channel();

        // Spawn the background thread that fetches the replay list periodically.
        thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            loop {
                // Lock the settings to get the current server address and refresh interval.
                let (server_addr, refresh_interval) = {
                    let s = settings_clone.lock().unwrap();
                    (s.server_addr.clone(), s.refresh_interval)
                };

                // Build the URL for the list endpoint.
                let list_url = format!("{}/list", server_addr);
                match client.get(&list_url).send() {
                    Ok(response) => {
                        if let Ok(replays) = response.json::<Vec<Replay>>() {
                            if tx.send(replays).is_err() {
                                // The UI thread has closed; exit the thread.
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
                thread::sleep(Duration::from_secs(refresh_interval));
            }
        });

        Self {
            replays: Vec::new(),
            list_rx: rx,
            settings,
            current_page: Page::Replays,
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Top panel for navigation
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

        // Process any new replay lists coming from the background thread.
        while let Ok(new_replays) = self.list_rx.try_recv() {
            self.replays = new_replays;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_page {
                Page::Replays => {
                    ui.heading("Replay Viewer");
                    ui.separator();

                    // Put the replay list inside a scroll area.
                    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        for replay in &self.replays {
                            ui.group(|ui| {
                                // A horizontal layout for the friendly name and download button.
                                ui.horizontal(|ui| {
                                    ui.label(format!("Friendly Name: {}", replay.friendlyName));
                                    if ui.button("Download").clicked() {
                                        // When clicked, spawn a thread to call the download endpoint.
                                        let replay_id = replay._id.clone();
                                        // Also capture the current server address.
                                        let server_addr = {
                                            let s = self.settings.lock().unwrap();
                                            s.server_addr.clone()
                                        };
                                        thread::spawn(move || {
                                            let download_url =
                                                format!("{}/download/{}", server_addr, replay_id);
                                            match reqwest::blocking::get(&download_url) {
                                                Ok(resp) => {
                                                    println!(
                                                        "Downloaded replay {}: {:?}",
                                                        replay_id, resp
                                                    );
                                                }
                                                Err(err) => {
                                                    eprintln!(
                                                        "Error downloading {}: {}",
                                                        replay_id, err
                                                    );
                                                }
                                            }
                                        });
                                    }
                                });
                                // Show the list of players/users.
                                ui.label(format!("Users: {:?}", replay.users));
                                // Additional fields can be added here as desired.
                            });
                            ui.add_space(10.0);
                        }
                    });
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
                    } else {
                        ui.label("Error accessing settings");
                    }
                }
            }
        });

        // Request a repaint to keep the UI updating.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Replay Viewer",
        options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc)))),
    )
}
