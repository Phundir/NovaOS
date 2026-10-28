//! USB descriptors -- field-by-field decoding.
//!
//! Everything a device says about itself arrives as descriptors: a series
//! of self-describing records, each preceded by its length and its type.
//! Nothing is aligned, nothing is guaranteed: a device may announce a
//! wrong length, an unknown type, or a configuration overflowing the
//! buffer.
//!
//! So the decoding is done **byte by byte**, with a bound check on every
//! record, exactly as for the firmware tables of `acpi.rs` and the
//! headers of `binfmt`. A malformed descriptor makes the device be given
//! up, never the kernel crash.

/// Descriptor types (USB 2.0, table 9-5).
pub mod kind {
    pub const DEVICE: u8 = 1;
    pub const CONFIGURATION: u8 = 2;
    pub const INTERFACE: u8 = 4;
    pub const ENDPOINT: u8 = 5;
    pub const HID: u8 = 0x21;
    pub const REPORT: u8 = 0x22;
}

/// HID class (USB HID 1.11, 4.1).
pub const CLASS_HID: u8 = 3;
/// "Boot interface" subclass: the device accepts the simplified protocol
/// the BIOS can read (4.2).
pub const SUBCLASS_BOOT: u8 = 1;
/// Mouse protocol (4.3).
pub const PROTOCOL_MOUSE: u8 = 2;

/// Device descriptor (table 9-8).
#[derive(Debug, Clone, Copy, Default)]
pub struct Device {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Packet size of endpoint 0. For USB 3, the field carries the
    /// exponent: the size is 2 to that power.
    pub max_packet0: u16,
    pub vendor: u16,
    pub product: u16,
    pub configurations: u8,
}

impl Device {
    pub fn parse(data: &[u8]) -> Option<Device> {
        if data.len() < 18 || data[1] != kind::DEVICE {
            return None;
        }
        let usb_version = u16::from_le_bytes([data[2], data[3]]);
        let raw = data[7] as u16;
        // USB 3 encodes the exponent, USB 2 the size. The threshold is the
        // version announced: a 3.x device with "9" means 512, not nine
        // bytes.
        let max_packet0 = if usb_version >= 0x0300 {
            1u16 << raw.min(15)
        } else {
            raw
        };
        Some(Device {
            usb_version,
            class: data[4],
            subclass: data[5],
            protocol: data[6],
            max_packet0,
            vendor: u16::from_le_bytes([data[8], data[9]]),
            product: u16::from_le_bytes([data[10], data[11]]),
            configurations: data[17],
        })
    }
}

/// Interface kept in a configuration, with its input endpoint.
#[derive(Debug, Clone, Copy, Default)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Address of the interrupt IN endpoint, bit 7 set.
    pub endpoint: u8,
    pub max_packet: u16,
    /// Polling interval, as the descriptor expresses it.
    pub interval: u8,
    /// Length of the HID report descriptor, zero if there is none.
    pub report_length: u16,
}

impl Interface {
    /// Endpoint number without the direction bit.
    pub fn endpoint_number(&self) -> u8 {
        self.endpoint & 0x0F
    }

    /// xHCI endpoint identifier: `2 * number + direction`.
    ///
    /// The formula comes from 4.5.1 of the xHCI specification. An IN and
    /// an OUT of the same number occupy two distinct contexts, and
    /// confusing them would arm the wrong one.
    pub fn xhci_endpoint_id(&self) -> u8 {
        self.endpoint_number() * 2 + 1
    }

    pub fn is_boot_mouse(&self) -> bool {
        self.class == CLASS_HID
            && self.subclass == SUBCLASS_BOOT
            && self.protocol == PROTOCOL_MOUSE
    }

    pub fn is_hid(&self) -> bool {
        self.class == CLASS_HID
    }
}

/// Decoded configuration: what we keep of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Configuration {
    pub value: u8,
    pub total_length: u16,
    pub interfaces: u8,
    /// HID interface kept, if there is one.
    pub hid: Option<Interface>,
}

/// Total length announced by a configuration descriptor.
///
/// Read separately because it decides the size of the second transfer:
/// we first ask for the first nine bytes, then for exactly what they
/// announce. Asking for an arbitrary size would either truncate a rich
/// configuration or claim more than the device has.
pub fn configuration_total_length(data: &[u8]) -> Option<u16> {
    if data.len() < 9 || data[1] != kind::CONFIGURATION {
        return None;
    }
    Some(u16::from_le_bytes([data[2], data[3]]))
}

/// Walks a complete configuration and keeps its HID interface.
///
/// The walk is linear: each record carries its length in the first byte.
/// A null length would stop progress and loop forever -- so it makes the
/// walk give up.
///
/// The endpoint kept is the **first interrupt IN** met after a HID
/// interface. That is the layout 4.4 of the HID describes: a mouse
/// exposes one incoming interrupt endpoint, and nothing else mandatory.
pub fn parse_configuration(data: &[u8]) -> Option<Configuration> {
    if data.len() < 9 || data[1] != kind::CONFIGURATION {
        return None;
    }

    let mut config = Configuration {
        value: data[5],
        total_length: u16::from_le_bytes([data[2], data[3]]),
        interfaces: data[4],
        hid: None,
    };

    let mut offset = data[0] as usize;
    let mut current: Option<Interface> = None;

    while offset + 2 <= data.len() {
        let length = data[offset] as usize;
        let descriptor = data[offset + 1];
        if length < 2 || offset + length > data.len() {
            break;
        }
        let record = &data[offset..offset + length];

        match descriptor {
            kind::INTERFACE if length >= 9 => {
                // The previous interface is kept if it was complete: a
                // second interface ends the first.
                if let Some(interface) = current
                    && interface.endpoint != 0
                    && config.hid.is_none()
                {
                    config.hid = Some(interface);
                }
                let interface = Interface {
                    number: record[2],
                    alternate: record[3],
                    class: record[5],
                    subclass: record[6],
                    protocol: record[7],
                    ..Interface::default()
                };
                // Only the HID interfaces interest us; the others are
                // stepped over to reach the next.
                current = if interface.is_hid() {
                    Some(interface)
                } else {
                    None
                };
            }

            kind::HID if length >= 9 => {
                if let Some(interface) = current.as_mut() {
                    // The HID descriptor announces the length of the
                    // report descriptor in its first sub-record of type
                    // 0x22.
                    let count = record[5] as usize;
                    for index in 0..count {
                        let base = 6 + index * 3;
                        if base + 3 > length {
                            break;
                        }
                        if record[base] == kind::REPORT {
                            interface.report_length =
                                u16::from_le_bytes([record[base + 1], record[base + 2]]);
                            break;
                        }
                    }
                }
            }

            kind::ENDPOINT if length >= 7 => {
                if let Some(interface) = current.as_mut()
                    && interface.endpoint == 0
                {
                    let address = record[2];
                    let attributes = record[3];
                    // Interrupt (type 3) and incoming (bit 7 of the address).
                    if attributes & 0b11 == 3 && address & 0x80 != 0 {
                        interface.endpoint = address;
                        interface.max_packet =
                            u16::from_le_bytes([record[4], record[5]]) & 0x07FF;
                        interface.interval = record[6];
                    }
                }
            }

            _ => {}
        }

        offset += length;
    }

    if let Some(interface) = current
        && interface.endpoint != 0
        && config.hid.is_none()
    {
        config.hid = Some(interface);
    }

    Some(config)
}
