use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, AdapterEvent, Address, Device};
use futures::StreamExt;
use tokio::time::timeout;

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

/// Inateck scanners ship with an unregistered OUI, so they cannot be identified
/// by a vendor lookup. Their addresses do share this prefix.
const INATECK_ADDR_PREFIX: [u8; 2] = [0xAB, 0x2B];

/// Which advertising devices a scan should report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanFilter {
    /// Every device currently advertising.
    #[default]
    All,
    /// Only devices whose address matches [`INATECK_ADDR_PREFIX`].
    InateckOnly,
}

impl ScanFilter {
    fn accepts(self, addr: Address) -> bool {
        match self {
            Self::All => true,
            Self::InateckOnly => addr[..2] == INATECK_ADDR_PREFIX,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScannedDevice {
    pub address: Address,
    pub name: Option<String>,
    /// Always known: a device is only reported once it has advertised.
    pub rssi: i16,
}

impl ScannedDevice {
    /// Whether this looks like an Inateck scanner, judged by address prefix.
    pub fn is_inateck(&self) -> bool {
        ScanFilter::InateckOnly.accepts(self.address)
    }
}

impl std::fmt::Display for ScannedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.name.as_deref().unwrap_or("<unknown>");
        write!(f, "{} {} (RSSI: {} dBm)", self.address, name, self.rssi)
    }
}

#[derive(Debug, Clone)]
pub struct GattCharInfo {
    pub uuid: bluer::Uuid,
    pub flags: bluer::gatt::CharacteristicFlags,
    pub value: Option<Vec<u8>>,
    pub descriptors: Vec<bluer::Uuid>,
}

#[derive(Debug, Clone)]
pub struct GattServiceInfo {
    pub uuid: bluer::Uuid,
    pub primary: bool,
    pub characteristics: Vec<GattCharInfo>,
}

pub async fn create_adapter() -> Result<(bluer::Session, Adapter)> {
    let session = bluer::Session::new()
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
    Ok((session, adapter))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(bytes: [u8; 6]) -> Address {
        Address::new(bytes)
    }

    #[test]
    fn all_accepts_any_address() {
        assert!(ScanFilter::All.accepts(addr([0xAB, 0x2B, 0x01, 0x02, 0x03, 0x04])));
        assert!(ScanFilter::All.accepts(addr([0xCC, 0x98, 0x8B, 0xB1, 0x72, 0x68])));
    }

    #[test]
    fn inateck_only_matches_the_ab2b_prefix() {
        assert!(ScanFilter::InateckOnly.accepts(addr([0xAB, 0x2B, 0x00, 0x00, 0x00, 0x00])));
        assert!(ScanFilter::InateckOnly.accepts(addr([0xAB, 0x2B, 0xFF, 0xFF, 0xFF, 0xFF])));
    }

    #[test]
    fn inateck_only_rejects_near_misses() {
        // Right first byte, wrong second.
        assert!(!ScanFilter::InateckOnly.accepts(addr([0xAB, 0x2C, 0x01, 0x02, 0x03, 0x04])));
        // Right second byte, wrong first.
        assert!(!ScanFilter::InateckOnly.accepts(addr([0xAA, 0x2B, 0x01, 0x02, 0x03, 0x04])));
        // Prefix present, but not at the start.
        assert!(!ScanFilter::InateckOnly.accepts(addr([0x00, 0xAB, 0x2B, 0x02, 0x03, 0x04])));
        // A real device that leaked into scans before the RSSI fix.
        assert!(!ScanFilter::InateckOnly.accepts(addr([0xCC, 0x98, 0x8B, 0xB1, 0x72, 0x68])));
    }
}

pub async fn scan_devices(
    adapter: &Adapter,
    duration: Duration,
    filter: ScanFilter,
    mut on_device: impl FnMut(ScannedDevice),
) -> Result<()> {
    // Properties are fetched asynchronously and are usually not populated yet at
    // DeviceAdded. This variant re-emits a device whenever they change, giving the
    // RSSI check below a chance to see the real value.
    let discover = adapter
        .discover_devices_with_changes()
        .await
        .context("failed to start discovery")?;
    tokio::pin!(discover);

    // Running out the timeout is the normal way a scan ends.
    let _ = timeout(duration, async {
        let mut seen = HashSet::new();
        while let Some(event) = discover.next().await {
            if let AdapterEvent::DeviceAdded(addr) = event
                && !seen.contains(&addr)
                && filter.accepts(addr)
                && let Ok(device) = adapter.device(addr)
            {
                // BlueZ replays every device it still remembers, whether in range or
                // not. Only one advertising during this scan has an RSSI.
                let Ok(Some(rssi)) = device.rssi().await else {
                    continue;
                };
                seen.insert(addr);
                let name = device.name().await.ok().flatten();
                on_device(ScannedDevice {
                    address: addr,
                    name,
                    rssi,
                });
            }
        }
    })
    .await;

    Ok(())
}

pub async fn connect(adapter: &Adapter, addr: Address) -> Result<Device> {
    let device = adapter
        .device(addr)
        .context("device not found - run 'scan' first")?;

    if !device.is_connected().await? {
        let _discover = adapter.discover_devices().await.ok();
        tokio::time::sleep(Duration::from_secs(2)).await;

        device
            .connect()
            .await
            .context("failed to connect to device")?;

        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    Ok(device)
}

pub async fn discover_services(device: &Device) -> Result<Vec<GattServiceInfo>> {
    let mut services = Vec::new();

    for service in device.services().await? {
        let uuid = service.uuid().await?;
        let primary = service.primary().await.unwrap_or(false);
        let mut characteristics = Vec::new();

        for char in service.characteristics().await? {
            let char_uuid = char.uuid().await?;
            let flags = char.flags().await?;

            let value = if flags.read {
                char.read().await.ok()
            } else {
                None
            };

            let descriptors: Vec<bluer::Uuid> = char
                .descriptors()
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|d| futures::executor::block_on(d.uuid()).ok())
                .collect();

            characteristics.push(GattCharInfo {
                uuid: char_uuid,
                flags,
                value,
                descriptors,
            });
        }

        services.push(GattServiceInfo {
            uuid,
            primary,
            characteristics,
        });
    }

    Ok(services)
}

pub async fn find_notify_characteristic(
    device: &Device,
    filter_uuid: Option<&str>,
) -> Result<(Characteristic, String)> {
    for service in device.services().await? {
        for char in service.characteristics().await? {
            let uuid = char.uuid().await?;
            let flags = char.flags().await?;

            if let Some(target) = filter_uuid {
                if uuid.to_string() == target && flags.notify {
                    return Ok((char, uuid.to_string()));
                }
            } else if flags.notify {
                let uuid_str = uuid.to_string();
                if CANDIDATE_UUIDS.contains(&uuid_str.as_str()) {
                    return Ok((char, uuid_str));
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
                    return Ok((char, uuid.to_string()));
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "no suitable notify characteristic found - run 'discover' to inspect available characteristics"
    ))
}

/// Buffer BLE notification chunks and yield complete barcode scans.
/// A scan is considered complete after `flush_delay` of silence.
pub async fn read_barcodes(
    characteristic: &Characteristic,
    flush_delay: Duration,
    mut on_barcode: impl FnMut(Vec<u8>),
) -> Result<()> {
    let notify_stream = characteristic
        .notify()
        .await
        .context("failed to subscribe to notifications")?;
    tokio::pin!(notify_stream);

    let mut buffer = Vec::<u8>::new();

    loop {
        let chunk = timeout(flush_delay, notify_stream.next()).await;

        match chunk {
            Ok(Some(value)) => {
                buffer.extend_from_slice(&value);
            }
            Ok(None) => break,
            Err(_) if !buffer.is_empty() => {
                on_barcode(std::mem::take(&mut buffer));
            }
            Err(_) => {}
        }
    }

    Ok(())
}
