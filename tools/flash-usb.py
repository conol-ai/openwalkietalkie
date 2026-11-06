#!/usr/bin/env python3
"""Flash the Rev A or Rev B application over the deck's own USB-C port, through
the bootloader's recovery mode (native/bootloader). No probe, no test pads,
no driver.

    tools/flash-usb.py target/thumbv8m.main-none-eabihf/release/openwalkietalkie-fw
    tools/flash-usb.py firmware.bin            # a raw image for 0x12020000
    tools/flash-usb.py --status                # what is attached, and which mode

Recovery preserves the deck's vendor HID interface ("OpenWalkieTalkie",
1209:5754). New bootloaders add a bulk interface; auto mode prefers it when
PyUSB/libusb is installed, otherwise using compatible channel-9 HID reports.
Empty sectors use short erase requests when supported. Both paths transfer
CRC-checked 4 KB sectors into the application window only; the bootloader writes the
vector-table sector last, checks it, and reboots into the application, whose
version this script then reports.

Flow: a running deck is asked over channel 2 to reboot into recovery
(`sys.reboot`, mode recovery). A deck that cannot run its application
(erased or half-written image) boots into recovery by itself; so does one
powered on with SHIFT and CANCEL (ESC) held. Needs the `hid` Python module.

The application names its board (`sys.version` answers `board`, and the ELF
carries the same name), and the three boards' images share one build path:
an ELF built for another board than the running deck reports is refused
unless --any-board is passed. A deck already in recovery cannot say which
board it is.
"""
import argparse
import base64
from collections import deque
import json
import secrets
import struct
import sys
import time
import zlib

VID, PID, USAGE_PAGE = 0x1209, 0x5754, 0xFF00
APP_BASE = 0x12020000
APP_END = 0x13000000
BONDS_START, BONDS_END = 0x12700000, 0x12720000
SECTOR = 4096
REPORT_ID, RPC_CHANNEL, RECOVERY_CHANNEL, PAYLOAD_MAX = 6, 2, 9, 61
OP_HELLO, OP_BEGIN, OP_DATA, OP_COMMIT, OP_FINISH, OP_BOOT, OP_READ = 1, 2, 3, 4, 5, 6, 7
STATUS = {0: "ok", 1: "bad arguments", 2: "crc mismatch", 3: "flash error", 4: "verify error"}


def log(message):
    print(f"flash-usb: {message}", flush=True)


def fail(message):
    print(f"flash-usb: {message}", file=sys.stderr)
    sys.exit(1)


# ---- image -------------------------------------------------------------------

def image_from_elf(data, xip=False):
    """The flash contents an ELF describes: its PT_LOAD segments by physical
    address inside the application window, gaps filled with 0xFF (what sftool
    does when it flashes an ELF).

    Every board executes from NOR; Rev A images before 0.13 linked code at
    PSRAM. Both layouts are accepted (downgrades), with board identity
    checked independently.
    `xip` is retained for compatibility with old callers; it is no longer an
    override for an unidentified/dev-board image."""
    if len(data) < 52 or data[:7] != b"\x7fELF\x01\x01\x01":
        fail("not a 32-bit little-endian ELF")
    e_phoff = struct.unpack_from("<I", data, 0x1C)[0]
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 0x2A)
    if e_phentsize < 32 or e_phoff + e_phentsize * e_phnum > len(data):
        fail("invalid ELF program headers")
    segments = []
    executable = False
    for i in range(e_phnum):
        off = e_phoff + i * e_phentsize
        p_type, p_offset, p_vaddr, p_paddr, p_filesz, _memsz, flags, _align = struct.unpack_from("<IIIIIIII", data, off)
        if p_type != 1 or p_filesz == 0:
            continue
        if p_offset + p_filesz > len(data):
            fail("truncated ELF segment")
        if p_paddr < APP_BASE or p_paddr + p_filesz > APP_END:
            if 0x20000000 <= p_paddr < 0x70000000:
                continue  # RAM/PSRAM-only segment
            fail(f"segment at 0x{p_paddr:08X} ({p_filesz} bytes) is outside the application window")
        if p_paddr < BONDS_END and p_paddr + p_filesz > BONDS_START:
            fail("ELF segment overlaps protected Bluetooth storage")
        segments.append((p_paddr, data[p_offset:p_offset + p_filesz]))
        executable |= bool(flags & 1) and (p_vaddr == p_paddr or 0x60000000 <= p_vaddr < 0x60800000)
    if not segments:
        fail("no flash segments in the ELF")
    if not executable:
        fail("no executable flash or PSRAM application segment")
    segments.sort()
    base = segments[0][0]
    if base != APP_BASE:
        fail(f"the image starts at 0x{base:08X}, expected the vector table at 0x{APP_BASE:08X}")
    end = max(addr + len(body) for addr, body in segments)
    image = bytearray(b"\xff" * (end - base))
    previous = base
    for addr, body in segments:
        if addr < previous:
            fail("overlapping flash segments")
        image[addr - base:addr - base + len(body)] = body
        previous = addr + len(body)
    if image_board(image) is None:
        fail("image must identify exactly one board: Rev A or Rev B (not the LilyGo dev-board build)")
    return bytes(image)


def image_board(data):
    """The board an application image names in its boot banner (src/board.rs
    `NAME`), or None for a raw image that carries none."""
    boards = [name for name in ("Rev A", "Rev B") if b"OpenWalkieTalkie " + name.encode() + b" (" in data]
    return boards[0] if len(boards) == 1 else None


def load_image(path, xip=False):
    with open(path, "rb") as source:
        data = source.read()
    image = image_from_elf(data, xip) if data[:4] == b"\x7fELF" else data
    if len(image) < 8:
        fail("image is too short for an application vector table")
    sp, pc = struct.unpack_from("<II", image, 0)
    if not (0x20000000 <= sp <= 0x20080000) or not (pc & 1) or not (APP_BASE <= (pc & ~1) < APP_BASE + len(image)):
        fail(f"the image's vector table looks wrong (SP 0x{sp:08X}, reset 0x{pc:08X})")
    if len(image) > APP_END - APP_BASE:
        fail("image larger than the application window")
    if BONDS_START <= (pc & ~1) < BONDS_END:
        fail("reset vector points into protected Bluetooth storage")
    validate_padding(image)
    if image_board(image) is None:
        fail("image must identify exactly one board: Rev A or Rev B")
    return image


def validate_padding(image):
    if not 8 <= len(image) <= APP_END - APP_BASE:
        fail("invalid application image size")
    if any(byte != 255 for byte in image[BONDS_START - APP_BASE:BONDS_END - APP_BASE]):
        fail("protected Bluetooth storage must be FF padding")


def check_recovery(image, hello):
    """Validate before OP_BEGIN erases the installed application's vectors."""
    validate_padding(image)
    if not hello or hello[0] not in (1, 2):
        fail("unsupported recovery protocol")
    if hello[0] == 1 and APP_BASE + len(image) > BONDS_START:
        fail("this image needs the protocol-2 recovery bootloader; upgrade recovery first (see native/README.md; application was not erased)")


# ---- the deck's USB device --------------------------------------------------------

def hid_module():
    try:
        import hid  # noqa: PLC0415
    except ImportError:
        fail("the 'hid' Python module is missing for this interpreter "
             f"({sys.executable}); with the SiFli SDK environment sourced, run /usr/bin/python3 tools/flash-usb.py")
    return hid


def device_paths(serial=None):
    """The deck's vendor collections on USB, optionally one deck by serial.

    A deck paired over Bluetooth shows up in the same enumeration (bus type 2,
    no serial, interface -1) and must never be used: recovery is USB-only, so
    a reboot request sent over that road would strand a deck that is not on
    the cable in recovery mode.
    """
    hid = hid_module()
    paths = []
    for d in hid.enumerate(VID, PID):
        if d.get("usage_page") != USAGE_PAGE:
            continue
        if d.get("bus_type", 1) != 1 or d.get("interface_number", 0) < 0:
            continue
        if serial is not None and d.get("serial_number") != serial:
            continue
        paths.append(d["path"])
    return paths


class Link:
    """One open vendor HID collection: reports of [id 6][channel][len][payload]."""

    def __init__(self, path):
        hid = hid_module()
        self.dev = hid.device()
        self.dev.open_path(path)
        self.dev.set_nonblocking(0)
        self.serial = self.dev.get_serial_number_string()
        self.capabilities = 0

    def close(self):
        self.dev.close()

    def send(self, channel, payload):
        if len(payload) > PAYLOAD_MAX:
            raise ValueError("payload too long")
        report = bytes([REPORT_ID, channel, len(payload)]) + bytes(payload)
        self.dev.write(report + b"\0" * (64 - len(report)))

    def recv(self, channel, timeout_s):
        """The next payload on `channel`, or None; other channels are skipped."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            data = bytes(self.dev.read(64, timeout_ms=100))
            if not data:
                continue
            body = data[1:] if data[0] == REPORT_ID else data
            if len(body) < 2 or body[0] != channel:
                continue
            n = body[1]
            return body[2:2 + n]
        return None

    def rpc(self, method, params, timeout_s=3.0):
        request = json.dumps({"method": method, "params": params, "id": 1}, separators=(",", ":")).encode() + b"\n"
        for off in range(0, len(request), PAYLOAD_MAX):
            self.send(RPC_CHANNEL, request[off:off + PAYLOAD_MAX])
        line = b""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            chunk = self.recv(RPC_CHANNEL, deadline - time.monotonic())
            if chunk is None:
                break
            line += chunk
            if b"\n" in line:
                try:
                    return json.loads(line.split(b"\n", 1)[0])
                except json.JSONDecodeError:
                    return {"raw": line.decode(errors="replace")}
        return None

    def hello(self, timeout_s=1.0):
        """(protocol, image valid) from a deck in recovery, else None."""
        self.send(RECOVERY_CHANNEL, [OP_HELLO])
        reply = self.recv(RECOVERY_CHANNEL, timeout_s)
        if reply and reply[:5] == bytes([OP_HELLO]) + b"OWTB" and len(reply) >= 7:
            self.capabilities = reply[7] if len(reply) >= 8 else 0
            return reply[5], bool(reply[6])
        return None

    def command(self, op, payload, timeout_s=5.0):
        self.send(RECOVERY_CHANNEL, [op] + list(payload))
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            reply = self.recv(RECOVERY_CHANNEL, deadline - time.monotonic())
            if reply is None:
                break
            if reply[0] == op:
                return reply[1:]
        fail(f"no reply to recovery command {op}")


def wait_for(predicate, what, timeout_s):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.3)
    fail(f"timed out waiting for {what}")


def open_recovery(known_paths, timeout_s, serial=None):
    """A link to the deck in recovery mode, once it has re-enumerated."""
    def fresh():
        for path in device_paths(serial):
            if path in known_paths:
                continue
            try:
                link = Link(path)
            except OSError:
                return None
            if link.hello():
                return link
            link.close()
        return None
    return wait_for(fresh, "the deck's recovery mode", timeout_s)


class BulkLink:
    """Optional PyUSB/libusb transport; claims only vendor interface 1."""
    def __init__(self, serial):
        import usb.core
        import usb.util
        self.util = usb.util
        self.dev = next((d for d in usb.core.find(find_all=True, idVendor=VID, idProduct=PID)
                         if d.serial_number == serial), None)
        if self.dev is None:
            raise RuntimeError("selected deck's bulk interface not found")
        intf = self.dev.get_active_configuration()[(1, 0)]
        if (intf.bInterfaceClass, intf.bInterfaceSubClass, intf.bInterfaceProtocol) != (255, 0x57, 1):
            raise RuntimeError("unexpected recovery bulk interface")
        if not all(any(e.bEndpointAddress == addr and e.bmAttributes & 3 == 2
                       and e.wMaxPacketSize == 64 for e in intf) for addr in (3, 0x85)):
            raise RuntimeError("unexpected recovery bulk endpoints")
        usb.util.claim_interface(self.dev, 1)

    def send(self, frame):
        if self.dev.write(3, frame, timeout=10000) != len(frame):
            fail("incomplete bulk write; retry from recovery")

    def recv(self):
        return bytes(self.dev.read(0x85, 64, timeout=10000))

    def close(self):
        self.util.dispose_resources(self.dev)


def bulk_frame(op, address, sequence, token, data=b""):
    header = struct.pack("<4sBBHIIII", b"OWTF", op, 1, len(data), address,
                         sequence, token, zlib.crc32(data))
    frame = header + struct.pack("<II", zlib.crc32(header), 0)
    if op == OP_COMMIT:
        frame += data + b"\xff" * (-len(data) % 64)
    return frame


def check_bulk_reply(reply, frame):
    if (len(reply) != 24 or reply[:4] != b"OWTA" or reply[4] != frame[4]
            or reply[6:8] != b"\x01\x00" or reply[8:16] != frame[12:20]
            or reply[16:20] != frame[8:12]
            or reply[20:] != struct.pack("<I", zlib.crc32(reply[:20]))):
        fail("invalid or stale bulk acknowledgment; retry from recovery")
    if reply[5]:
        fail(f"bulk command {reply[4]}: {STATUS.get(reply[5], reply[5])}; retry from recovery")


def download_bulk(bulk, image):
    token = secrets.randbits(32) or 1
    frame = bulk_frame(OP_BEGIN, len(image), 0, token)
    for attempt in range(2):
        try:
            bulk.send(frame)
            deadline = time.monotonic() + 10
            while True:
                reply = bulk.recv()
                stale = (len(reply) == 24 and reply[:4] == b"OWTA" and reply[6:8] == b"\x01\x00"
                         and reply[12:16] != frame[16:20]
                         and reply[20:] == struct.pack("<I", zlib.crc32(reply[:20])))
                if not stale:
                    check_bulk_reply(reply, frame)
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError("old bulk replies did not drain")
            break
        except OSError:
            if attempt:
                raise
            # Only BEGIN may be repeated: its short packet may have flushed a
            # previous incomplete frame, and no new DATA has been sent yet.
    pending = deque()
    sectors = iter((offset, image[offset:offset + SECTOR]) for offset in range(0, len(image), SECTOR)
                   if not BONDS_START <= APP_BASE + offset < BONDS_END)
    sequence, wire_bytes, last_percent = 1, (attempt + 1) * len(frame), -1
    started = time.monotonic()
    while True:
        while len(pending) < 2:
            item = next(sectors, None)
            if item is None:
                break
            offset, data = item
            frame = bulk_frame(8 if data == b"\xff" * len(data) else OP_COMMIT,
                               APP_BASE + offset, sequence, token, data)
            bulk.send(frame)
            wire_bytes += len(frame)
            pending.append((frame[:32], offset + len(data)))
            sequence += 1
        if not pending:
            break
        frame, done = pending.popleft()
        check_bulk_reply(bulk.recv(), frame)
        percent = min(99, done * 100 // len(image))
        if percent != last_percent:
            log(f"{percent:3d}% ({done}/{len(image)} address-span bytes verified)")
            last_percent = percent
    frame = bulk_frame(OP_FINISH, 0, sequence, token)
    bulk.send(frame)
    check_bulk_reply(bulk.recv(), frame)
    elapsed = time.monotonic() - started
    log(f"100%: bulk download done in {elapsed:.1f} s; {wire_bytes + len(frame)} uploaded bytes including framing; vectors verified, rebooting")


def download(link, image, transport="auto"):
    check_recovery(image, link.hello())
    capabilities = getattr(link, "capabilities", 0)
    if transport != "hid" and capabilities & 2:
        try:
            bulk = BulkLink(link.serial)
        except Exception as error:
            if transport == "bulk":
                fail(f"bulk unavailable before erase: {error}")
            log(f"bulk unavailable ({error}); using compatible HID recovery")
        else:
            log("using USB bulk with two queued sectors and sparse padding")
            try:
                download_bulk(bulk, image)
            finally:
                bulk.close()
            return
    elif transport == "bulk":
        fail("this bootloader does not advertise bulk recovery; application was not erased")
    # Optional BEGIN size; older recovery builds ignore the payload.
    status = link.command(OP_BEGIN, struct.pack("<I", len(image)))
    if status[0] != 0:
        fail(f"begin failed: {STATUS.get(status[0], status[0])}")
    sectors = range(0, len(image), SECTOR)
    started = time.monotonic()
    for index, offset in enumerate(sectors):
        if BONDS_START <= APP_BASE + offset < BONDS_END:
            continue  # FF is padding in the image, never an instruction to erase bonds.
        chunk = image[offset:offset + SECTOR]
        sparse = capabilities & 1 and chunk == b"\xff" * len(chunk)
        if not sparse:
            for off in range(0, len(chunk), PAYLOAD_MAX - 4):
                piece = chunk[off:off + PAYLOAD_MAX - 4]
                link.send(RECOVERY_CHANNEL, bytes([OP_DATA, off & 0xFF, off >> 8, len(piece)]) + piece)
        addr = APP_BASE + offset
        reply = link.command(8 if sparse else OP_COMMIT, struct.pack("<IHI", addr, len(chunk), zlib.crc32(chunk) & 0xFFFFFFFF), timeout_s=10)
        if reply[0] != 0 or struct.unpack_from("<I", reply, 1)[0] != addr:
            fail(f"sector 0x{addr:08X}: {STATUS.get(reply[0], reply[0])}")
        if index % 32 == 0 or offset + SECTOR >= len(image):
            done = min(offset + SECTOR, len(image))
            log(f"{done * 100 // len(image):3d}% ({done}/{len(image)} bytes)")
    reply = link.command(OP_FINISH, [], timeout_s=10)
    elapsed = time.monotonic() - started
    if reply[0] != 0 or not reply[1]:
        fail(f"finish: {STATUS.get(reply[0], reply[0])}, image valid: {bool(reply[1])}")
    log(f"download done in {elapsed:.1f} s ({len(image) / 1024 / max(elapsed, 0.001):.0f} KB/s); the bootloader checked the vector table and reboots")


def update_bootloader(link, path):
    """Bench only: hand a bootloader image to an application built with the
    `bootloader-update` feature, which writes it to its slot at 0x12010000
    (src/link/boot_update.rs). Not in --help on purpose: a power loss while
    the slot is rewritten leaves only the UART pads to put one back."""
    image = open(path, "rb").read()
    if not 8 <= len(image) <= 0x10000:
        fail(f"{path}: {len(image)} bytes; a bootloader is 8 B .. 64 KiB")
    sp, pc = struct.unpack_from("<II", image)
    if not (0x20000000 <= sp <= 0x20080000 and pc & 1 and 0x20000000 <= pc <= 0x20080000):
        fail(f"{path}: vector table SP 0x{sp:08X} PC 0x{pc:08X} does not point into SRAM; not a bootloader")
    crc = zlib.crc32(image)
    reply = link.rpc("boot.begin", {"n": len(image), "crc": crc})
    if not reply or "result" not in reply:
        fail(f"boot.begin refused: {reply} (is the application built with --features hw-a,bootloader-update?)")
    started = time.monotonic()
    for off in range(0, len(image), 128):
        reply = link.rpc("boot.data", {"o": off, "d": base64.b64encode(image[off:off + 128]).decode()})
        if not reply or reply.get("result", {}).get("o") != min(off + 128, len(image)):
            fail(f"boot.data at {off} failed: {reply}")
    log(f"bootloader: {len(image)} bytes staged in RAM in {time.monotonic() - started:.1f} s; writing the slot")
    reply = link.rpc("boot.commit", {}, timeout_s=30)
    result = (reply or {}).get("result")
    if not result:
        fail(f"boot.commit failed: {reply}")
    if result["crc"] != crc:
        fail(f"the slot reads back CRC 0x{result['crc']:08X}, not 0x{crc:08X}: rewrite it over UART before rebooting")
    if result["written"] == 0:
        log("bootloader: the slot already holds this image; nothing written")
    else:
        log(f"bootloader: {result['written']} bytes written and read back (CRC 0x{crc:08X})")
    return result["written"]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("image", nargs="?", help="application ELF (load segments) or raw .bin for 0x12020000")
    parser.add_argument("--status", action="store_true", help="say what is attached and in which mode, then exit")
    parser.add_argument("--no-request", action="store_true", help="the deck is already in recovery (buttons, or no valid image)")
    parser.add_argument("--no-wait-app", action="store_true", help="do not wait for the application to come back")
    # Legacy no-op: identified Rev A/B XIP images are accepted by default.
    parser.add_argument("--xip", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--any-board", action="store_true",
                        help="flash although the image names another board than the running application does")
    parser.add_argument("--bootloader", metavar="BIN", help=argparse.SUPPRESS)
    parser.add_argument("--serial", help="the deck to use when more than one is on USB (the USB serial number)")
    parser.add_argument("--transport", choices=("auto", "hid", "bulk"), default="auto",
                        help="auto prefers bulk when supported; hid keeps the compatible transport")
    args = parser.parse_args()

    if args.bootloader:
        paths = device_paths(args.serial)
        if not paths:
            fail("the deck is not attached")
        link = Link(paths[0])
        if link.hello():
            link.close()
            fail("the deck is in recovery mode; the bootloader is replaced from the running application")
        written = update_bootloader(link, args.bootloader)
        if written:
            log(f"rebooting so the new bootloader runs: {link.rpc('sys.reboot', {})}")
        link.close()
        return

    paths = device_paths(args.serial)
    if args.status:
        if not paths:
            print("deck: not attached" + (f" (serial {args.serial})" if args.serial else ""))
            return
        link = Link(paths[0])
        hello = link.hello()
        if hello:
            print(f"deck: recovery mode (protocol {hello[0]}, application image {'valid' if hello[1] else 'INVALID'})")
        else:
            print("deck: application,", link.rpc("sys.version", {}))
        link.close()
        return
    if not args.image:
        parser.error("an image is required (or --status)")
    image = load_image(args.image, args.xip)
    board = image_board(image)
    log(f"image: {len(image)} bytes for 0x{APP_BASE:08X}" + (f", a {board} build" if board else ""))

    if not paths:
        fail("the deck is not attached")
    link = Link(paths[0])
    selected_serial = link.serial
    if not link.hello():
        if args.no_request:
            link.close()
            fail("the deck is running its application, not recovery mode")
        # The running application knows its board; an image for the other one
        # boots, with its speaker silent and its charger unread.
        version = link.rpc("sys.version", {}) or {}
        deck_board = (version.get("result") or {}).get("board") if isinstance(version, dict) else None
        if board and deck_board and deck_board != board:
            if not args.any_board:
                link.close()
                fail(f"the deck is a {deck_board} board and this image is a {board} build; "
                     "build with the matching feature, or pass --any-board")
            log(f"WARNING: writing a {board} build to a {deck_board} board (--any-board)")
        elif board and not deck_board:
            log("the running application does not name its board (older firmware); not checked")
        reply = link.rpc("sys.reboot", {"mode": "recovery"})
        link.close()
        log(f"asked the application to reboot into recovery: {reply}")
        link = open_recovery(set(paths), 30, selected_serial)
    log("deck is in recovery mode; downloading")
    download(link, image, args.transport)
    seen = set(device_paths(selected_serial))
    link.close()
    if args.no_wait_app:
        return
    log("waiting for the application to boot")

    def application():
        for path in device_paths(selected_serial):
            if path in seen:
                continue
            try:
                probe = Link(path)
            except OSError:
                return None
            version = probe.rpc("sys.version", {}, timeout_s=1.5)
            probe.close()
            if version:
                return version
        return None
    version = wait_for(application, "the application's HID device", 40)
    log(f"application is back: {version}")


if __name__ == "__main__":
    main()
