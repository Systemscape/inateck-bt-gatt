use std::sync::mpsc;
use std::time::Duration;

use bluer::Address;
use eframe::egui;
use inateck_bt_gatt::{
    connect, create_adapter, find_notify_characteristic, format_barcode_output, parse_iso15434,
    read_barcodes, scan_devices, ParsedBarcode, ScanFilter, ScannedDevice,
};

enum BleCommand {
    Scan,
    Connect(Address),
    StartReading(Address, Option<String>),
    StopReading,
}

enum BleEvent {
    DeviceFound(ScannedDevice),
    ScanComplete,
    Connected(Address),
    Subscribed(String),
    BarcodeReceived { raw: Vec<u8>, parsed: Option<ParsedBarcode> },
    Error(String),
    ReadingStopped,
}

struct App {
    cmd_tx: mpsc::Sender<BleCommand>,
    event_rx: mpsc::Receiver<BleEvent>,

    devices: Vec<ScannedDevice>,
    /// View filter only — the scan itself always collects every device, so
    /// toggling this re-filters instantly instead of forcing a rescan.
    inateck_only: bool,
    selected_device: Option<usize>,
    connected_addr: Option<Address>,
    scanning: bool,
    connecting: bool,
    reading: bool,

    scans: Vec<ScanEntry>,
    status: String,

    clipboard: Option<arboard::Clipboard>,
}

struct ScanEntry {
    parsed: Option<ParsedBarcode>,
    raw_text: String,
}

impl App {
    fn new(cmd_tx: mpsc::Sender<BleCommand>, event_rx: mpsc::Receiver<BleEvent>) -> Self {
        Self {
            cmd_tx,
            event_rx,
            devices: Vec::new(),
            inateck_only: false,
            selected_device: None,
            connected_addr: None,
            scanning: false,
            connecting: false,
            reading: false,
            scans: Vec::new(),
            status: "Ready".to_string(),
            clipboard: arboard::Clipboard::new().ok(),
        }
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                BleEvent::DeviceFound(dev) => {
                    self.devices.push(dev);
                }
                BleEvent::ScanComplete => {
                    self.scanning = false;
                    self.status = format!("Scan complete: {} devices", self.devices.len());
                }
                BleEvent::Connected(addr) => {
                    self.connecting = false;
                    self.connected_addr = Some(addr);
                    self.status = format!("Connected to {}", addr);
                }
                BleEvent::Subscribed(uuid) => {
                    self.reading = true;
                    self.status = format!("Listening on {}", uuid);
                }
                BleEvent::BarcodeReceived { raw, parsed } => {
                    let raw_text = String::from_utf8_lossy(&raw).to_string();
                    self.scans.push(ScanEntry { parsed, raw_text });
                    self.status = format!("Received scan #{}", self.scans.len());
                }
                BleEvent::Error(msg) => {
                    self.scanning = false;
                    self.connecting = false;
                    self.reading = false;
                    self.status = format!("Error: {}", msg);
                }
                BleEvent::ReadingStopped => {
                    self.reading = false;
                    self.status = "Reading stopped".to_string();
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
            });
        });

        egui::SidePanel::left("devices_panel")
            .min_width(280.0)
            .show(ctx, |ui| {
                ui.heading("Devices");
                ui.separator();

                ui.horizontal(|ui| {
                    let scan_label = if self.scanning { "Scanning..." } else { "Scan" };
                    if ui
                        .add_enabled(!self.scanning, egui::Button::new(scan_label))
                        .clicked()
                    {
                        self.devices.clear();
                        self.selected_device = None;
                        self.scanning = true;
                        self.status = "Scanning...".to_string();
                        let _ = self.cmd_tx.send(BleCommand::Scan);
                    }

                    // A selection that the filter just hid would otherwise keep
                    // driving the Connect/Read buttons below.
                    if ui
                        .checkbox(&mut self.inateck_only, "Inateck only")
                        .on_hover_text("Show only devices with an AB:2B:* address")
                        .changed()
                        && self.inateck_only
                        && let Some(idx) = self.selected_device
                        && !self.devices[idx].is_inateck()
                    {
                        self.selected_device = None;
                    }
                });

                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        let inateck_only = self.inateck_only;
                        let mut shown = 0;
                        // enumerate() before filter() keeps `i` an index into
                        // self.devices, which selected_device refers to.
                        for (i, dev) in self
                            .devices
                            .iter()
                            .enumerate()
                            .filter(|(_, dev)| !inateck_only || dev.is_inateck())
                        {
                            shown += 1;
                            let name = dev.name.as_deref().unwrap_or("<unknown>");
                            let label = format!("{} {}", dev.address, name);
                            let selected = self.selected_device == Some(i);
                            if ui.selectable_label(selected, &label).clicked() {
                                self.selected_device = Some(i);
                            }
                        }

                        let hidden = self.devices.len() - shown;
                        if hidden > 0 {
                            ui.weak(format!("{} device(s) hidden by filter", hidden));
                        }
                    });

                ui.separator();

                if let Some(idx) = self.selected_device {
                    let addr = self.devices[idx].address;
                    let is_connected = self.connected_addr == Some(addr);

                    ui.horizontal(|ui| {
                        if !is_connected {
                            let connect_label = if self.connecting {
                                "Connecting..."
                            } else {
                                "Connect"
                            };
                            if ui
                                .add_enabled(!self.connecting, egui::Button::new(connect_label))
                                .clicked()
                            {
                                self.connecting = true;
                                self.status = format!("Connecting to {}...", addr);
                                let _ = self.cmd_tx.send(BleCommand::Connect(addr));
                            }
                        } else if !self.reading {
                            if ui.button("Start Reading").clicked() {
                                self.status = "Starting reader...".to_string();
                                let _ = self.cmd_tx.send(BleCommand::StartReading(addr, None));
                            }
                        } else if ui.button("Stop Reading").clicked() {
                            let _ = self.cmd_tx.send(BleCommand::StopReading);
                        }
                    });
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Barcode Scans");
            ui.separator();

            if self.scans.is_empty() {
                ui.label("No scans yet. Connect to the scanner and scan a barcode.");
                return;
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let scan_count = self.scans.len();
                    for (i, scan) in self.scans.iter().enumerate() {
                        ui.push_id(i, |ui| {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(format!("#{}", i + 1));
                                    if let Some(ref parsed) = scan.parsed {
                                        if let Some(pn) = parsed.supplier_pn() {
                                            ui.label(pn);
                                        }
                                        if let Some(qty) = parsed.quantity() {
                                            ui.label(format!("Qty: {}", qty));
                                        }
                                    }
                                    if ui.small_button("Copy raw").clicked()
                                        && let Some(ref mut c) = self.clipboard
                                    {
                                        let _ = c.set_text(&scan.raw_text);
                                    }
                                });

                                if let Some(ref parsed) = scan.parsed {
                                    ui.monospace(format_barcode_output(parsed));
                                } else {
                                    ui.monospace(&scan.raw_text);
                                }
                            });
                        });

                        if i + 1 < scan_count {
                            ui.add_space(4.0);
                        }
                    }
                });
        });
    }
}

fn spawn_ble_runtime(cmd_rx: mpsc::Receiver<BleCommand>, event_tx: mpsc::Sender<BleEvent>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().ok();
        let Some(rt) = rt else {
            let _ = event_tx.send(BleEvent::Error("failed to create tokio runtime".to_string()));
            return;
        };

        rt.block_on(async {
            let adapter = match create_adapter().await {
                Ok((_session, adapter)) => adapter,
                Err(e) => {
                    let _ = event_tx.send(BleEvent::Error(format!("Adapter: {}", e)));
                    return;
                }
            };

            // Keep session alive for the lifetime of the thread
            let mut stop_tx: Option<tokio::sync::oneshot::Sender<()>> = None;

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    BleCommand::Scan => {
                        let tx = event_tx.clone();
                        scan_devices(&adapter, Duration::from_secs(10), ScanFilter::All, |dev| {
                            let _ = tx.send(BleEvent::DeviceFound(dev));
                        })
                        .await
                        .ok();
                        let _ = event_tx.send(BleEvent::ScanComplete);
                    }
                    BleCommand::Connect(addr) => match connect(&adapter, addr).await {
                        Ok(_) => {
                            let _ = event_tx.send(BleEvent::Connected(addr));
                        }
                        Err(e) => {
                            let _ = event_tx.send(BleEvent::Error(format!("Connect: {}", e)));
                        }
                    },
                    BleCommand::StartReading(addr, char_uuid) => {
                        if let Some(tx) = stop_tx.take() {
                            let _ = tx.send(());
                        }

                        let device = match connect(&adapter, addr).await {
                            Ok(d) => d,
                            Err(e) => {
                                let _ =
                                    event_tx.send(BleEvent::Error(format!("Connect: {}", e)));
                                continue;
                            }
                        };

                        let (char, uuid) =
                            match find_notify_characteristic(&device, char_uuid.as_deref()).await {
                                Ok(r) => r,
                                Err(e) => {
                                    let _ = event_tx
                                        .send(BleEvent::Error(format!("Characteristic: {}", e)));
                                    continue;
                                }
                            };

                        let _ = event_tx.send(BleEvent::Subscribed(uuid));

                        let (stx, mut srx) = tokio::sync::oneshot::channel();
                        stop_tx = Some(stx);

                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let read_fut = read_barcodes(
                                &char,
                                Duration::from_millis(150),
                                |raw| {
                                    let parsed = parse_iso15434(&raw);
                                    let _ = tx.send(BleEvent::BarcodeReceived { raw, parsed });
                                },
                            );

                            tokio::select! {
                                result = read_fut => {
                                    if let Err(e) = result {
                                        let _ = tx.send(BleEvent::Error(format!("Read: {}", e)));
                                    }
                                    let _ = tx.send(BleEvent::ReadingStopped);
                                }
                                _ = &mut srx => {
                                    let _ = tx.send(BleEvent::ReadingStopped);
                                }
                            }
                        });
                    }
                    BleCommand::StopReading => {
                        if let Some(tx) = stop_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            }
        });
    });
}

fn main() -> eframe::Result {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    spawn_ble_runtime(cmd_rx, event_tx);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 500.0])
            .with_title("Inateck BT-GATT Scanner"),
        ..Default::default()
    };

    eframe::run_native(
        "Inateck BT-GATT Scanner",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(cmd_tx, event_rx)))),
    )
}
