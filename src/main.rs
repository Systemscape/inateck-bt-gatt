use std::time::Duration;

use anyhow::{Context, Result};
use bluer::{gatt::remote::Characteristic, Adapter, Address, AdapterEvent, Device};
use clap::{Parser, Subcommand};
use tokio::time::timeout;
use futures::StreamExt;

#[derive(Parser)]
#[command(name = "inateck-bt-gatt")]
#[command(about = "Read Inateck BCST-47 barcode scanner via BLE GATT (bypassing HID)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan for nearby BLE devices (look for the BCST-47)
    Scan {
        /// Scan duration in seconds
        #[arg(short, long, default_value_t = 10)]
        duration: u64,
    },
    /// Discover all GATT services and characteristics of a device
    Discover {
        /// Bluetooth device address (e.g. AA:BB:CC:DD:EE:FF)
        #[arg(short, long)]
        address: String,
    },
    /// Connect to the scanner and read barcode data via GATT notifications
    Read {
        /// Bluetooth device address (e.g. AA:BB:CC:DD:EE:FF)
        #[arg(short, long)]
        address: String,

        /// GATT characteristic UUID to subscribe to (auto-detected if omitted)
        #[arg(short, long)]
        characteristic: Option<String>,

        /// Copy each scanned barcode to the system clipboard
        #[arg(long, short = 'C')]
        clipboard: bool,

        /// Dump raw hex bytes alongside parsed output
        #[arg(long)]
        hex: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let session =
        bluer::Session::new()
            .await
            .context("failed to create BlueZ session")?;
    let adapter = session
        .default_adapter()
        .await
        .context("no Bluetooth adapter found")?;
    adapter
        .set_powered(true)
        .await
        .context("failed to power on adapter")?;

    match cli.command {
        Command::Scan { duration } => cmd_scan(&adapter, duration).await,
        Command::Discover { address } => {
            let addr = parse_address(&address)?;
            cmd_discover(&adapter, addr).await
        }
        Command::Read {
            address,
            characteristic,
            clipboard,
            hex,
        } => {
            let addr = parse_address(&address)?;
            cmd_read(&adapter, addr, characteristic, clipboard, hex).await
        }
    }
}

fn parse_address(s: &str) -> Result<Address> {
    s.parse::<Address>()
        .map_err(|e| anyhow::anyhow!("invalid Bluetooth address '{}': {}", s, e))
}

async fn cmd_scan(adapter: &Adapter, duration_secs: u64) -> Result<()> {
    println!(
        "Scanning for BLE devices on {} for {}s...\n",
        adapter.name(),
        duration_secs
    );

    let discover = adapter
        .discover_devices()
        .await
        .context("failed to start discovery")?;
    tokio::pin!(discover);

    let scan_result = timeout(Duration::from_secs(duration_secs), async {
        let mut seen = std::collections::HashSet::new();
        while let Some(event) = discover.next().await {
            if let AdapterEvent::DeviceAdded(addr) = event
                && seen.insert(addr)
                && let Ok(device) = adapter.device(addr)
            {
                let name = device.name().await.ok().flatten();
                let rssi = device.rssi().await.ok().flatten();
                let name_str = name.as_deref().unwrap_or("<unknown>");
                let rssi_str = rssi
                    .map(|r| format!("{} dBm", r))
                    .unwrap_or_else(|| "N/A".to_string());
                println!("  {} {} (RSSI: {})", addr, name_str, rssi_str);
            }
        }
    })
    .await;

    if scan_result.is_err() {
        println!("\nScan complete.");
    }

    Ok(())
}

async fn ensure_connected(adapter: &Adapter, addr: Address) -> Result<Device> {
    let device = adapter
        .device(addr)
        .context("device not found - run 'scan' first")?;

    if !device.is_connected().await? {
        println!("Connecting to {}...", addr);
        let _discover = adapter.discover_devices().await.ok();
        tokio::time::sleep(Duration::from_secs(2)).await;

        device
            .connect()
            .await
            .context("failed to connect to device")?;
        println!("Connected.");

        println!("Waiting for GATT service discovery...");
        tokio::time::sleep(Duration::from_secs(3)).await;
    } else {
        println!("Already connected to {}.", addr);
    }

    Ok(device)
}

async fn cmd_discover(adapter: &Adapter, addr: Address) -> Result<()> {
    let device = ensure_connected(adapter, addr).await?;

    println!("\nGATT Services:");
    println!("{}", "=".repeat(60));

    for service in device.services().await? {
        let uuid = service.uuid().await?;
        let primary = service.primary().await.unwrap_or(false);
        println!(
            "\nService: {} {}",
            uuid,
            if primary { "(primary)" } else { "" }
        );

        for char in service.characteristics().await? {
            let char_uuid = char.uuid().await?;
            let flags = char.flags().await?;
            println!("  Characteristic: {}", char_uuid);
            println!("    Flags: {:?}", flags);

            if flags.read {
                match char.read().await {
                    Ok(value) => {
                        let hex: String =
                            value.iter().map(|b| format!("{:02x} ", b)).collect();
                        let text = String::from_utf8_lossy(&value);
                        println!("    Value (hex): {}", hex.trim());
                        println!("    Value (txt): {}", text);
                    }
                    Err(e) => println!("    Read error: {}", e),
                }
            }

            for desc in char.descriptors().await.unwrap_or_default() {
                let desc_uuid = desc.uuid().await?;
                println!("    Descriptor: {}", desc_uuid);
            }
        }
    }

    Ok(())
}

// ISO/IEC 15434 Format 06 parser (Digi-Key / Mouser 2D barcode format)
const GS: u8 = 0x1D;
const RS: u8 = 0x1E;
const EOT: u8 = 0x04;

struct ParsedBarcode {
    fields: Vec<(String, String)>,
}

impl ParsedBarcode {
    fn get(&self, label: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    }
}

/// ASC MH10.8.2 Data Identifiers used by Digi-Key/Mouser (longest prefix first)
const DATA_IDENTIFIERS: &[(&str, &str)] = &[
    ("30P", "Original PN"),
    ("20Z", "Mutual (20Z)"),
    ("21Z", "Mutual (21Z)"),
    ("13Z", "Mutual (13Z)"),
    ("12Z", "Mutual (12Z)"),
    ("11Z", "Mutual (11Z)"),
    ("10K", "Invoice Number"),
    ("11K", "Packing List"),
    ("1P", "Manufacturer PN"),
    ("0P", "Supplier PN (alt)"),
    ("1K", "Order Number"),
    ("9D", "Date"),
    ("1T", "Lot/Trace"),
    ("4L", "Country"),
    ("1V", "Manufacturer"),
    ("2P", "Part Number (2P)"),
    ("P", "Supplier PN"),
    ("Q", "Quantity"),
    ("K", "Purchase Order"),
    ("C", "Internal (C)"),
    ("S", "Serial Number"),
    ("V", "Supplier"),
    ("H", "Internal (H)"),
];

fn identify_field(raw: &str) -> (String, String) {
    for (prefix, label) in DATA_IDENTIFIERS {
        if let Some(value) = raw.strip_prefix(prefix) {
            return (label.to_string(), value.to_string());
        }
    }
    ("Unknown".to_string(), raw.to_string())
}

fn parse_iso15434(raw: &[u8]) -> Option<ParsedBarcode> {
    // Strip [)> header
    let data = if raw.starts_with(b"[)>") {
        &raw[3..]
    } else {
        return None;
    };

    // Strip RS before format ID and EOT/RS trailer
    let data = data.strip_prefix(&[RS]).unwrap_or(data);
    let data = data.strip_suffix(&[0x0D]).unwrap_or(data);
    let data = data.strip_suffix(&[EOT]).unwrap_or(data);
    let data = data.strip_suffix(&[RS]).unwrap_or(data);

    // Expect format 06
    if !data.starts_with(b"06") {
        return None;
    }
    let data = &data[2..];
    let data = data.strip_prefix(&[GS]).unwrap_or(data);

    let fields: Vec<(String, String)> = data
        .split(|&b| b == GS)
        .filter_map(|segment| {
            let text = String::from_utf8_lossy(segment);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(identify_field(trimmed))
        })
        .collect();

    if fields.is_empty() {
        return None;
    }

    Some(ParsedBarcode { fields })
}

fn format_barcode_output(parsed: &ParsedBarcode) -> String {
    let max_label = parsed
        .fields
        .iter()
        .map(|(l, _)| l.len())
        .max()
        .unwrap_or(0);

    parsed
        .fields
        .iter()
        .map(|(label, value)| format!("  {:width$}  {}", label, value, width = max_label))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Well-known UUIDs for barcode scanner data characteristics (highest priority first)
const CANDIDATE_UUIDS: &[&str] = &[
    // Inateck/HPRT ST-47AI Expert mode barcode output
    "00002af0-0000-1000-8000-00805f9b34fb",
    // Inateck/HPRT Keyboard mode barcode output
    "0000ffe1-0000-1000-8000-00805f9b34fb",
    // Nordic UART Service TX characteristic
    "6e400003-b5a3-f393-e0a9-e50e24dcca9e",
    // HID Report characteristic
    "00002a4d-0000-1000-8000-00805f9b34fb",
    // Common barcode scanner characteristics
    "0000fff1-0000-1000-8000-00805f9b34fb",
    "0000ff01-0000-1000-8000-00805f9b34fb",
];

async fn find_notify_characteristic(
    device: &Device,
    filter_uuid: Option<&str>,
) -> Result<Characteristic> {
    for service in device.services().await? {
        for char in service.characteristics().await? {
            let uuid = char.uuid().await?;
            let flags = char.flags().await?;

            if let Some(target) = filter_uuid {
                if uuid.to_string() == target && flags.notify {
                    return Ok(char);
                }
            } else if flags.notify {
                let uuid_str = uuid.to_string();
                if CANDIDATE_UUIDS.contains(&uuid_str.as_str()) {
                    println!("Auto-detected notify characteristic: {}", uuid_str);
                    return Ok(char);
                }
            }
        }
    }

    if filter_uuid.is_none() {
        for service in device.services().await? {
            for char in service.characteristics().await? {
                let flags = char.flags().await?;
                if flags.notify {
                    let uuid = char.uuid().await?;
                    println!(
                        "No known characteristic found, using first notify characteristic: {}",
                        uuid
                    );
                    return Ok(char);
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "no suitable notify characteristic found - run 'discover' to inspect available characteristics"
    ))
}

async fn cmd_read(
    adapter: &Adapter,
    addr: Address,
    characteristic_uuid: Option<String>,
    clipboard: bool,
    hex: bool,
) -> Result<()> {
    let device = ensure_connected(adapter, addr).await?;

    let char =
        find_notify_characteristic(&device, characteristic_uuid.as_deref()).await?;
    let uuid = char.uuid().await?;

    let mut clip = if clipboard {
        let c = arboard::Clipboard::new().context("failed to open clipboard")?;
        println!("Clipboard mode enabled.");
        Some(c)
    } else {
        None
    };

    println!("\nSubscribing to characteristic {}...", uuid);
    println!("Waiting for barcode scans (Ctrl+C to quit):\n");

    let notify_stream = char
        .notify()
        .await
        .context("failed to subscribe to notifications")?;
    tokio::pin!(notify_stream);

    let flush_delay = Duration::from_millis(150);
    let mut buffer = Vec::<u8>::new();

    loop {
        let chunk = tokio::time::timeout(flush_delay, notify_stream.next()).await;

        match chunk {
            Ok(Some(value)) => {
                buffer.extend_from_slice(&value);
            }
            Ok(None) => break,
            Err(_) if !buffer.is_empty() => {
                if hex {
                    println!("[hex] {} bytes:", buffer.len());
                    for (i, chunk) in buffer.chunks(20).enumerate() {
                        let hex_part: String =
                            chunk.iter().map(|b| format!("{:02x} ", b)).collect();
                        let ascii_part: String = chunk
                            .iter()
                            .map(|&b| match b {
                                GS => '|',
                                RS => '^',
                                EOT => '$',
                                0x20..=0x7E => b as char,
                                _ => '.',
                            })
                            .collect();
                        println!(
                            "  {:04x}: {:<60} {}",
                            i * 20,
                            hex_part.trim_end(),
                            ascii_part
                        );
                    }
                    println!();
                }

                if let Some(parsed) = parse_iso15434(&buffer) {
                    println!("{}", format_barcode_output(&parsed));

                    // Copy raw barcode data with GS separators preserved
                    // (Part-DB / InvenTree parse ISO 15434 natively)
                    if let Some(ref mut c) = clip {
                        let raw_str = String::from_utf8_lossy(&buffer);
                        if let Err(e) = c.set_text(raw_str.as_ref()) {
                            eprintln!("clipboard error: {}", e);
                        } else {
                            let label = parsed
                                .get("Supplier PN")
                                .or_else(|| parsed.get("Manufacturer PN"))
                                .unwrap_or("barcode");
                            println!(
                                "  -> raw barcode for '{}' copied to clipboard",
                                label
                            );
                        }
                    }
                    println!();
                } else {
                    let text = String::from_utf8_lossy(&buffer);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        println!("{}", trimmed);
                        if let Some(ref mut c) = clip
                            && let Err(e) = c.set_text(trimmed)
                        {
                            eprintln!("clipboard error: {}", e);
                        }
                    } else {
                        let hex: String =
                            buffer.iter().map(|b| format!("{:02x} ", b)).collect();
                        println!("[raw] {}", hex.trim());
                    }
                }
                buffer.clear();
            }
            Err(_) => {}
        }
    }

    println!("Notification stream ended.");
    Ok(())
}
