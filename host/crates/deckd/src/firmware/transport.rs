//! The resident OWTB recovery protocol (native/bootloader): application
//! window only, CRC per sector, vector sector committed last by the device.
use super::{
    firmware_version, migration, validate_layout, Device, APP_BASE, BONDS_END, BONDS_START,
};
use crate::hid::{PID, USAGE_PAGE, VID};
use hidapi::{BusType, HidApi, HidDevice};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const SECTOR: usize = 4096;
const RECOVERY: u8 = 9;

struct Wire(HidDevice);
impl Wire {
    fn send(&self, channel: u8, payload: &[u8]) -> Result<(), String> {
        if payload.len() > 61 {
            return Err("USB payload too large.".into());
        }
        let mut report = [0; 64];
        report[0] = 6;
        report[1] = channel;
        report[2] = payload.len() as u8;
        report[3..3 + payload.len()].copy_from_slice(payload);
        match self.0.write(&report) {
            Ok(64) => Ok(()),
            _ => Err("USB write failed. Reconnect the deck and retry.".into()),
        }
    }
    fn recv(&self, channel: u8, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        while Instant::now() < deadline {
            let mut report = [0; 64];
            let n = self
                .0
                .read_timeout(&mut report, 100)
                .map_err(|_| "USB connection lost. Reconnect and retry.")?;
            if n == 0 {
                continue;
            }
            let body = if report[0] == 6 {
                &report[1..n]
            } else {
                &report[..n]
            };
            if body.len() < 2 || body[0] != channel {
                continue;
            }
            let len = body[1] as usize;
            if len > 61 || len > body.len() - 2 {
                return Err("Malformed USB report.".into());
            }
            return Ok(Some(body[2..2 + len].to_vec()));
        }
        Ok(None)
    }
    fn hello(&self) -> Result<Option<u8>, String> {
        Ok(self.hello_info()?.map(|(protocol, _)| protocol))
    }
    fn hello_info(&self) -> Result<Option<(u8, u8)>, String> {
        self.send(RECOVERY, &[1])?;
        let until = Instant::now() + Duration::from_millis(500);
        while let Some(reply) = self.recv(RECOVERY, until)? {
            if reply.starts_with(b"\x01OWTB") {
                if reply.len() < 7 || !matches!(reply[5], 1 | 2) {
                    return Err("Unsupported recovery protocol.".into());
                }
                return Ok(Some((reply[5], reply.get(7).copied().unwrap_or(0))));
            }
        }
        Ok(None)
    }
    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let request = format!("{}\n", json!({"method":method,"params":params,"id":47201}));
        for chunk in request.as_bytes().chunks(61) {
            self.send(2, chunk)?;
        }
        let until =
            Instant::now() + Duration::from_secs(if method == "boot.commit" { 30 } else { 2 });
        let mut line = Vec::new();
        while let Some(bytes) = self.recv(2, until)? {
            line.extend(bytes);
            if line.len() > 4096 {
                return Err("Oversized device reply.".into());
            }
            if let Some(reply) = take_reply(&mut line, 47201) {
                return reply.get("result").cloned().ok_or_else(|| {
                    reply["error"]["message"]
                        .as_str()
                        .unwrap_or("Device refused the update request.")
                        .to_owned()
                });
            }
        }
        Err("The deck did not answer. Connect its USB cable and try again.".into())
    }
    fn command(&self, op: u8, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut payload = vec![op];
        payload.extend_from_slice(data);
        self.send(RECOVERY, &payload)?;
        let until = Instant::now() + Duration::from_secs(10);
        while let Some(reply) = self.recv(RECOVERY, until)? {
            if reply.first() == Some(&op) {
                if reply.get(1) != Some(&0) {
                    return Err(format!(
                        "Recovery command {op} failed. Reconnect and retry."
                    ));
                }
                return Ok(reply[2..].to_vec());
            }
        }
        Err("Recovery timed out. Keep the deck on USB and retry the update.".into())
    }
}

/// The reply with `id` among the channel-2 lines gathered in `line`, once
/// its line feed is in; `None` until then. Every other whole line is
/// dropped: a reply to someone else's request (the service asks a linked
/// deck its board and charge on this channel too, `device::status`), or the
/// tail of one that began before this handle was reading, which is no
/// reply at all.
fn take_reply(line: &mut Vec<u8>, id: u64) -> Option<Value> {
    while let Some(end) = line.iter().position(|byte| *byte == b'\n') {
        let reply = serde_json::from_slice::<Value>(&line[..end]).ok();
        line.drain(..=end);
        let ours = |reply: &Value| reply.get("id").and_then(Value::as_u64) == Some(id);
        if let Some(reply) = reply.filter(ours) {
            return Some(reply);
        }
    }
    None
}

fn candidates(api: &mut HidApi) -> Result<Vec<(String, std::ffi::CString)>, String> {
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    api.refresh_devices().map_err(|e| e.to_string())?;
    Ok(api
        .device_list()
        .filter(|d| {
            d.vendor_id() == VID
                && d.product_id() == PID
                && d.usage_page() == USAGE_PAGE
                && matches!(d.bus_type(), BusType::Usb)
        })
        .filter_map(|d| {
            d.serial_number()
                .filter(|s| !s.is_empty())
                .map(|s| (s.to_string(), d.path().to_owned()))
        })
        .collect())
}

fn describe(wire: &Wire, serial: String) -> Result<Device, String> {
    if let Some(protocol) = wire.hello()? {
        return Ok(Device {
            recovery_protocol: Some(protocol),
            serial,
            recovery: true,
            ..Default::default()
        });
    }
    let result = wire.rpc("sys.version", json!({}))?;
    let version = result
        .get("version")
        .and_then(Value::as_str)
        .ok_or("Firmware version missing.")?;
    if !version.ends_with("-openwalkietalkie") || firmware_version(version).is_none() {
        return Err("This updater supports the standard OpenWalkieTalkie firmware; this device runs a different image.".into());
    }
    let board = match result.get("board").and_then(Value::as_str) {
        Some("Rev A") => Some("hw-a".into()),
        Some("Rev B") => Some("hw-b".into()),
        None => None,
        _ => return Err("This device board is not supported by the USB updater.".into()),
    };
    Ok(Device {
        serial,
        board,
        version: Some(version.into()),
        recovery: false,
        recovery_protocol: result["recovery_protocol"]
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .filter(|p| matches!(p, 1 | 2)),
        maintenance: result["maintenance"].as_bool().unwrap_or(false),
    })
}

pub fn inspect(api: &mut HidApi) -> Result<Option<Device>, String> {
    let found = candidates(api)?;
    if found.len() > 1 {
        return Err("Connect only the deck you want to update by USB.".into());
    }
    let Some((serial, path)) = found.into_iter().next() else {
        return Ok(None);
    };
    let wire = Wire(api.open_path(&path).map_err(|e| e.to_string())?);
    describe(&wire, serial).map(Some)
}

fn open_serial(api: &mut HidApi, serial: &str) -> Result<Wire, String> {
    let path = candidates(api)?
        .into_iter()
        .find(|(s, _)| s == serial)
        .map(|(_, p)| p)
        .ok_or("The selected deck is no longer connected by USB.")?;
    Ok(Wire(api.open_path(&path).map_err(|e| e.to_string())?))
}

/// How long the deck may take to show up on USB again after it restarts,
/// into recovery or back into its firmware. Windows gets longer: the first
/// time it meets recovery's composite device it sets it up (and looks for a
/// driver for the bulk interface) before the HID side appears, and a deck
/// passed through to a virtual machine waits for the hypervisor to attach it
/// again. Measured on a QEMU guest (2026-11-14): 176 s from the end of the
/// transfer to the updated deck on the guest's bus.
const REAPPEAR_WITHIN: Duration = if cfg!(windows) {
    Duration::from_secs(180)
} else {
    Duration::from_secs(40)
};

fn wait<T>(mut f: impl FnMut() -> Option<T>, what: &str) -> Result<T, String> {
    let until = Instant::now() + REAPPEAR_WITHIN;
    loop {
        if let Some(value) = f() {
            return Ok(value);
        }
        if Instant::now() >= until {
            return Err(format!(
                "Timed out waiting for {what}. Reconnect USB and check again."
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub(super) fn install(
    api: &mut HidApi,
    request: migration::Request<'_>,
    images: &migration::Images,
    backup_dir: &Path,
    progress: impl FnMut(&str, u8, &str),
) -> Result<Device, String> {
    let serial = request.expected.serial.clone();
    let wire = Some(open_serial(api, &serial)?);
    let mut io = UsbMigration {
        api,
        serial,
        wire,
        backup_dir,
    };
    migration::run(&mut io, request, images, progress)
}

struct UsbMigration<'a> {
    api: &'a mut HidApi,
    serial: String,
    wire: Option<Wire>,
    backup_dir: &'a Path,
}

impl migration::Io for UsbMigration<'_> {
    fn describe(&mut self) -> Result<Device, String> {
        describe(
            self.wire.as_ref().ok_or("USB connection is closed.")?,
            self.serial.clone(),
        )
    }

    fn enter_recovery(&mut self) -> Result<u8, String> {
        let wire = self.wire.as_ref().ok_or("USB connection is closed.")?;
        if let Some(protocol) = wire.hello()? {
            return Ok(protocol);
        }
        wire.rpc("sys.reboot", json!({"mode":"recovery"}))?;
        self.wire.take();
        let wire = wait(
            || {
                let w = open_serial(self.api, &self.serial).ok()?;
                w.hello().ok()?.is_some().then_some(w)
            },
            "this deck's recovery mode",
        )?;
        let protocol = wire.hello()?.ok_or("The deck left recovery mode.")?;
        self.wire = Some(wire);
        Ok(protocol)
    }

    fn transfer(
        &mut self,
        image: &[u8],
        protocol: u8,
        progress: &mut dyn FnMut(u8),
    ) -> Result<(), String> {
        let wire = self.wire.as_ref().ok_or("USB connection is closed.")?;
        let (current, capabilities) = wire.hello_info()?.ok_or("The deck left recovery mode.")?;
        if current != protocol {
            return Err("Recovery protocol changed before transfer.".into());
        }
        if capabilities & 2 != 0 {
            match super::bulk::Usb::open(&self.serial) {
                Ok(mut usb) => return super::bulk::transfer(&mut usb, image, protocol, progress),
                Err(error) => {
                    tracing::warn!(%error, "Bulk unavailable; using compatible HID recovery")
                }
            }
        }
        transfer_with_capabilities(
            image,
            protocol,
            capabilities,
            |op, payload, reply| {
                if reply {
                    wire.command(op, payload)
                } else {
                    let mut bytes = vec![op];
                    bytes.extend_from_slice(payload);
                    wire.send(RECOVERY, &bytes)?;
                    Ok(Vec::new())
                }
            },
            progress,
        )
    }

    fn wait_application(&mut self) -> Result<Device, String> {
        self.wire.take();
        let (wire, device) = wait(
            || {
                let w = open_serial(self.api, &self.serial).ok()?;
                let d = describe(&w, self.serial.clone()).ok()?;
                (!d.recovery).then_some((w, d))
            },
            "the updated deck",
        )?;
        self.wire = Some(wire);
        Ok(device)
    }

    fn boot_existing(&mut self) -> Result<(), String> {
        self.wire
            .as_ref()
            .ok_or("USB connection is closed.")?
            .command(6, &[])?;
        self.wait_application().map(|_| ())
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.wire
            .as_ref()
            .ok_or("USB connection is closed.")?
            .rpc(method, params)
    }

    fn backup(&mut self, bytes: &[u8]) -> Result<(), String> {
        std::fs::create_dir_all(self.backup_dir)
            .map_err(|e| format!("Cannot save bootloader backup: {e}"))?;
        // Hash the serial instead of using externally supplied text as a path.
        let serial = format!("{:x}", Sha256::digest(self.serial.as_bytes()));
        let hash = format!("{:x}", Sha256::digest(bytes));
        let path = self.backup_dir.join(format!("{serial}-{hash}.bin"));
        let mut file = std::fs::File::create(&path)
            .map_err(|e| format!("Cannot save bootloader backup: {e}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("Cannot save bootloader backup: {e}"))?;
        if std::fs::read(path).map_err(|e| e.to_string())? != bytes {
            return Err("Bootloader backup verification failed.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
fn transfer(
    image: &[u8],
    protocol: u8,
    command: impl FnMut(u8, &[u8], bool) -> Result<Vec<u8>, String>,
    progress: impl FnMut(u8),
) -> Result<(), String> {
    transfer_with_capabilities(image, protocol, 0, command, progress)
}

fn transfer_with_capabilities(
    image: &[u8],
    protocol: u8,
    capabilities: u8,
    mut command: impl FnMut(u8, &[u8], bool) -> Result<Vec<u8>, String>,
    mut progress: impl FnMut(u8),
) -> Result<(), String> {
    preflight(image, protocol)?;
    // Optional BEGIN extension; older recovery builds ignore these bytes.
    // The resident screen counts verified sectors against the image span.
    command(2, &(image.len() as u32).to_le_bytes(), true)?;
    for (index, sector) in image.chunks(SECTOR).enumerate() {
        let addr = APP_BASE + (index * SECTOR) as u32;
        if (BONDS_START..BONDS_END).contains(&addr) {
            continue;
        }
        let sparse = capabilities & 1 != 0 && sector.iter().all(|b| *b == 0xff);
        if !sparse {
            for (part, bytes) in sector.chunks(57).enumerate() {
                let offset = (part * 57) as u16;
                let mut payload = offset.to_le_bytes().to_vec();
                payload.push(bytes.len() as u8);
                payload.extend_from_slice(bytes);
                command(3, &payload, false)?;
            }
        }
        let mut payload = addr.to_le_bytes().to_vec();
        payload.extend_from_slice(&(sector.len() as u16).to_le_bytes());
        payload.extend_from_slice(&crc32(sector).to_le_bytes());
        let reply = command(if sparse { 8 } else { 4 }, &payload, true)?;
        if reply.as_slice() != addr.to_le_bytes() {
            return Err("The bootloader acknowledged the wrong flash sector.".into());
        }
        progress(((index * SECTOR + sector.len()) * 100 / image.len()).min(99) as u8);
    }
    let reply = command(5, &[], true)?;
    if reply.first() != Some(&1) {
        return Err("The bootloader could not validate the new image. Retry from recovery.".into());
    }
    progress(100);
    Ok(())
}

pub(super) fn preflight(image: &[u8], protocol: u8) -> Result<(), String> {
    // Negotiate before OP_BEGIN erases the installed application's vectors.
    validate_layout(image)?;
    if !matches!(protocol, 1 | 2) {
        return Err("Unsupported recovery protocol.".into());
    }
    if protocol == 1 && APP_BASE + image.len() as u32 > BONDS_START {
        return Err("This firmware needs a recovery bootloader upgrade first. Follow the recovery upgrade procedure in native/README.md. The installed application has not been erased.".into());
    }
    Ok(())
}

pub(super) fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 16] = [
        0x00000000, 0x1db71064, 0x3b6e20c8, 0x26d930ac, 0x76dc4190, 0x6b6b51f4, 0x4db26158,
        0x5005713c, 0xedb88320, 0xf00f9344, 0xd6d6a3e8, 0xcb61b38c, 0x9b64c2b0, 0x86d3d2d4,
        0xa00ae278, 0xbdbdf21c,
    ];
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        crc = (crc >> 4) ^ TABLE[(crc & 15) as usize];
        crc = (crc >> 4) ^ TABLE[(crc & 15) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The updater reads channel 2 from a handle of its own while the
    /// service may be asking the same deck its status there: its reply is
    /// found by id among the service's replies, and the tail of one that
    /// began before the updater was reading is passed over, not taken for
    /// an unreadable answer.
    #[test]
    fn the_updaters_reply_is_found_among_the_services_and_a_cut_one_is_passed_over() {
        let mut line = Vec::new();
        let ours = r#"{"id":47201,"result":{"version":"0.13.0-openwalkietalkie","board":"Rev B"}}"#;
        line.extend_from_slice(b"\"battery\":85,\"is_charging\":false}}\n");
        line.extend_from_slice(br#"{"id":47302,"result":{"board":"Rev B"}}"#);
        line.push(b'\n');
        line.extend_from_slice(&ours.as_bytes()[..40]);
        assert_eq!(take_reply(&mut line, 47201), None, "not whole yet");
        assert_eq!(
            line,
            &ours.as_bytes()[..40],
            "only the partial reply is left"
        );
        line.extend_from_slice(&ours.as_bytes()[40..]);
        line.extend_from_slice(b"\n{\"id\":47301,");
        let reply = take_reply(&mut line, 47201).expect("the updater's own reply");
        assert_eq!(reply["result"]["board"], "Rev B");
        assert_eq!(
            line, b"{\"id\":47301,",
            "what follows waits for its line feed"
        );
    }

    #[test]
    fn sparse_hid_sends_erase_requests_only_when_advertised() {
        let image = vec![0xff; 4200];
        let mut ops = Vec::new();
        transfer_with_capabilities(
            &image,
            2,
            1,
            |op, data, _| {
                ops.push(op);
                Ok(match op {
                    8 => data[..4].to_vec(),
                    5 => vec![1],
                    _ => vec![],
                })
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(ops, [2, 8, 8, 5]);
    }
    #[test]
    fn expanded_update_skips_bonds_and_refuses_old_recovery_before_erase() {
        let mut image = vec![255; (BONDS_END - APP_BASE) as usize + 17];
        image[0] = 1;
        let mut addresses = Vec::new();
        transfer(
            &image,
            2,
            |op, data, _| {
                if op == 4 {
                    let address = u32::from_le_bytes(data[..4].try_into().unwrap());
                    assert!(!(BONDS_START..BONDS_END).contains(&address));
                    addresses.push(address);
                    return Ok(data[..4].to_vec());
                }
                Ok(if op == 5 { vec![1] } else { vec![] })
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(addresses.last(), Some(&BONDS_END));
        assert_eq!(
            addresses.len(),
            ((BONDS_START - APP_BASE) as usize / SECTOR) + 1
        );
        for protocol in [1, 3] {
            let mut calls = 0;
            assert!(transfer(
                &image,
                protocol,
                |_, _, _| {
                    calls += 1;
                    Ok(vec![])
                },
                |_| {}
            )
            .is_err());
            assert_eq!(calls, 0);
        }
        image[(BONDS_START - APP_BASE) as usize] = 0;
        let mut calls = 0;
        assert!(transfer(
            &image,
            2,
            |_, _, _| {
                calls += 1;
                Ok(vec![])
            },
            |_| {}
        )
        .is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn transfer_matches_sector_protocol_including_short_final_sector() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        assert_eq!(crc32(&[]), 0);
        assert_eq!(crc32(&[0xa5; 32]), 0x775f2f85);
        assert_eq!(crc32(&[0xa5; 104]), 0x497ecff0);
        assert_eq!(crc32(&[0xa5; 4096]), 0x4a9d36c6);
        let image = vec![0xa5; 4200];
        let mut offset = 0;
        let mut staged = Vec::new();
        let mut completed = false;
        transfer(
            &image,
            2,
            |op, data, reply| {
                match op {
                    2 => {
                        assert!(reply);
                        assert_eq!(data, (image.len() as u32).to_le_bytes());
                    }
                    3 => {
                        assert!(!reply);
                        assert_eq!(
                            u16::from_le_bytes(data[..2].try_into().unwrap()) as usize,
                            staged.len()
                        );
                        assert_eq!(data[2] as usize, data.len() - 3);
                        assert!(data.len() <= 60);
                        staged.extend_from_slice(&data[3..]);
                    }
                    4 => {
                        assert!(reply);
                        assert_eq!(
                            u32::from_le_bytes(data[..4].try_into().unwrap()),
                            APP_BASE + offset
                        );
                        assert_eq!(
                            u16::from_le_bytes(data[4..6].try_into().unwrap()) as usize,
                            staged.len()
                        );
                        assert_eq!(
                            u32::from_le_bytes(data[6..].try_into().unwrap()),
                            crc32(&staged)
                        );
                        assert_eq!(
                            &image[offset as usize..offset as usize + staged.len()],
                            staged
                        );
                        offset += staged.len() as u32;
                        staged.clear();
                        return Ok(data[..4].to_vec());
                    }
                    5 => {
                        assert_eq!(offset as usize, image.len());
                        completed = true;
                        return Ok(vec![1]);
                    }
                    _ => panic!(),
                }
                Ok(vec![])
            },
            |_| {},
        )
        .unwrap();
        assert!(completed);
    }
    #[test]
    fn failed_commit_never_finishes_or_boots() {
        let mut operations = Vec::new();
        let result = transfer(
            &[1; 12],
            2,
            |op, _, _| {
                operations.push(op);
                if op == 4 {
                    Err("CRC mismatch".into())
                } else {
                    Ok(vec![])
                }
            },
            |_| {},
        );
        assert!(result.is_err());
        assert!(!operations.contains(&5));
    }
}
