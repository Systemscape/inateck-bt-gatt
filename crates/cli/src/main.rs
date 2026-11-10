use std::time::Duration;

use anyhow::{Context, Result};
use bluer::Address;
use clap::{Parser, Subcommand};
use inateck_bt_gatt::{
    connect, create_adapter, discover_services, find_notify_characteristic,
    format_barcode_output, hex_dump, parse_iso15434, read_barcodes, scan_devices, ScanFilter,
};

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

        /// Only report Inateck scanners (address prefix AB:2B)
        #[arg(short, long)]
        inateck_only: bool,
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
    let (_session, adapter) = create_adapter().await?;

    match cli.command {
        Command::Scan {
            duration,
            inateck_only,
        } => {
            let filter = if inateck_only {
                ScanFilter::InateckOnly
            } else {
                ScanFilter::All
            };
            println!(
                "Scanning for {} on {} for {}s...\n",
                if inateck_only {
                    "Inateck scanners"
                } else {
                    "BLE devices"
                },
                adapter.name(),
                duration
            );
            scan_devices(&adapter, Duration::from_secs(duration), filter, |dev| {
                println!("  {}", dev);
            })
            .await?;
            println!("\nScan complete.");
        }
        Command::Discover { address } => {
            let addr = parse_address(&address)?;
            let device = connect(&adapter, addr).await?;
            println!("Connected to {}.", addr);

            println!("\nGATT Services:");
            println!("{}", "=".repeat(60));

            for service in discover_services(&device).await? {
                println!(
                    "\nService: {} {}",
                    service.uuid,
                    if service.primary { "(primary)" } else { "" }
                );
                for ch in &service.characteristics {
                    println!("  Characteristic: {}", ch.uuid);
                    println!("    Flags: {:?}", ch.flags);
                    if let Some(ref value) = ch.value {
                        let hex_str: String =
                            value.iter().map(|b| format!("{:02x} ", b)).collect();
                        let text = String::from_utf8_lossy(value);
                        println!("    Value (hex): {}", hex_str.trim());
                        println!("    Value (txt): {}", text);
                    }
                    for desc in &ch.descriptors {
                        println!("    Descriptor: {}", desc);
                    }
                }
            }
        }
        Command::Read {
            address,
            characteristic,
            clipboard,
            hex,
        } => {
            let addr = parse_address(&address)?;
            let device = connect(&adapter, addr).await?;
            println!("Connected to {}.", addr);

            let (char, char_uuid) =
                find_notify_characteristic(&device, characteristic.as_deref()).await?;
            println!("Subscribing to characteristic {}...", char_uuid);

            let mut clip = if clipboard {
                let c = arboard::Clipboard::new()
                    .context("failed to open clipboard")?;
                println!("Clipboard mode enabled.");
                Some(c)
            } else {
                None
            };

            println!("Waiting for barcode scans (Ctrl+C to quit):\n");

            read_barcodes(&char, Duration::from_millis(150), |raw| {
                if hex {
                    println!("{}\n", hex_dump(&raw));
                }

                if let Some(parsed) = parse_iso15434(&raw) {
                    println!("{}", format_barcode_output(&parsed));

                    if let Some(ref mut c) = clip {
                        let raw_str = String::from_utf8_lossy(&raw);
                        if let Err(e) = c.set_text(raw_str.as_ref()) {
                            eprintln!("clipboard error: {}", e);
                        } else {
                            println!(
                                "  -> raw barcode for '{}' copied to clipboard",
                                parsed.best_label()
                            );
                        }
                    }
                    println!();
                } else {
                    let text = String::from_utf8_lossy(&raw);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        println!("{}", trimmed);
                        if let Some(ref mut c) = clip
                            && let Err(e) = c.set_text(trimmed)
                        {
                            eprintln!("clipboard error: {}", e);
                        }
                    } else {
                        let hex_str: String =
                            raw.iter().map(|b| format!("{:02x} ", b)).collect();
                        println!("[raw] {}", hex_str.trim());
                    }
                }
            })
            .await?;

            println!("Notification stream ended.");
        }
    }

    Ok(())
}

fn parse_address(s: &str) -> Result<Address> {
    s.parse::<Address>()
        .map_err(|e| anyhow::anyhow!("invalid Bluetooth address '{}': {}", s, e))
}
