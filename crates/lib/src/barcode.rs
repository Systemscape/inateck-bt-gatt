const GS: u8 = 0x1D;
const RS: u8 = 0x1E;
const EOT: u8 = 0x04;

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

#[derive(Debug, Clone)]
pub struct ParsedBarcode {
    pub fields: Vec<(String, String)>,
}

impl ParsedBarcode {
    pub fn get(&self, label: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    }

    pub fn supplier_pn(&self) -> Option<&str> {
        self.get("Supplier PN")
    }

    pub fn manufacturer_pn(&self) -> Option<&str> {
        self.get("Manufacturer PN")
    }

    pub fn quantity(&self) -> Option<&str> {
        self.get("Quantity")
    }

    pub fn best_label(&self) -> &str {
        self.supplier_pn()
            .or_else(|| self.manufacturer_pn())
            .unwrap_or("barcode")
    }
}

fn identify_field(raw: &str) -> (String, String) {
    for (prefix, label) in DATA_IDENTIFIERS {
        if let Some(value) = raw.strip_prefix(prefix) {
            return (label.to_string(), value.to_string());
        }
    }
    ("Unknown".to_string(), raw.to_string())
}

/// Parse an ISO/IEC 15434 Format 06 barcode (Digi-Key / Mouser 2D DataMatrix)
pub fn parse_iso15434(raw: &[u8]) -> Option<ParsedBarcode> {
    let data = if raw.starts_with(b"[)>") {
        &raw[3..]
    } else {
        return None;
    };

    let data = data.strip_prefix(&[RS]).unwrap_or(data);
    let data = data.strip_suffix(&[0x0D]).unwrap_or(data);
    let data = data.strip_suffix(&[EOT]).unwrap_or(data);
    let data = data.strip_suffix(&[RS]).unwrap_or(data);

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

pub fn format_barcode_output(parsed: &ParsedBarcode) -> String {
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

pub fn hex_dump(buffer: &[u8]) -> String {
    let mut lines = vec![format!("[hex] {} bytes:", buffer.len())];
    for (i, chunk) in buffer.chunks(20).enumerate() {
        let hex_part: String = chunk.iter().map(|b| format!("{:02x} ", b)).collect();
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
        lines.push(format!(
            "  {:04x}: {:<60} {}",
            i * 20,
            hex_part.trim_end(),
            ascii_part
        ));
    }
    lines.join("\n")
}
