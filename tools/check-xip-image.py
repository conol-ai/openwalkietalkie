#!/usr/bin/env python3
"""Verify Embassy execution layout: every board executes in place from NOR."""
import pathlib
import struct
import argparse

APP_BASE = 0x12020000
APP_END = 0x13000000
BONDS_START, BONDS_END = 0x12700000, 0x12720000
PSRAM_START, PSRAM_SCRATCH = 0x60000000, 0x60700000
SRAM_START, SRAM_END = 0x20000000, 0x2007FC00


def check(path, bootstrap=False):
    data = pathlib.Path(path).read_bytes()
    assert data[:7] == b"\x7fELF\x01\x01\x01", "expected ELF32 little-endian"
    phoff, shoff = struct.unpack_from("<II", data, 28)
    phsize, phcount, shsize, shcount, strings = struct.unpack_from("<5H", data, 42)
    section_headers = [struct.unpack_from("<10I", data, shoff + i * shsize) for i in range(shcount)]
    names = section_headers[strings]
    names = data[names[4]:names[4] + names[5]]
    sections = {names[s[0]:].split(b"\0", 1)[0].decode(): s for s in section_headers}
    segments = [struct.unpack_from("<8I", data, phoff + i * phsize) for i in range(phcount)]
    segments = [s for s in segments if s[0] == 1 and s[4]]
    symbols = sections[".symtab"]
    strings = section_headers[symbols[6]]
    strings = data[strings[4]:strings[4] + strings[5]]
    symbol_values = {}
    functions = []
    for at in range(symbols[4], symbols[4] + symbols[5], symbols[9]):
        name, value, size, info, _, _ = struct.unpack_from("<IIIBBH", data, at)
        name = strings[name:].split(b"\0", 1)[0].decode()
        symbol_values[name] = value
        if info & 0xF == 2:
            functions.append((name, value, size))

    def section(name, start, end):
        assert name in sections, f"missing {name}"
        s = sections[name]
        assert s[5] and start <= s[3] < s[3] + s[5] <= end, f"{name} outside required execution region"
        assert s[3] % s[8] == 0, f"{name} alignment"
        return s

    def load_address(s):
        for _, offset, vaddr, paddr, size, _, _, _ in segments:
            if vaddr <= s[3] and s[3] + s[5] <= vaddr + size:
                assert s[4] == offset + s[3] - vaddr, "section/segment offset mismatch"
                return paddr + s[3] - vaddr
        raise AssertionError("section has no flash load image")

    vector = section(".vector_table", APP_BASE, BONDS_START)
    assert vector[3] == APP_BASE, "vector table must start the application"
    sp, reset = struct.unpack_from("<II", data, vector[4])
    assert SRAM_START <= sp <= SRAM_END and sp % 8 == 0 and reset & 1, "invalid reset vectors"
    text = section(".text", APP_BASE, BONDS_START)
    assert text[3] <= (reset & ~1) < text[3] + text[5], "Reset must execute in NOR"
    section(".rodata", APP_BASE, BONDS_START)
    section(".flash_assets", APP_BASE if bootstrap else BONDS_END, BONDS_START if bootstrap else APP_END)
    # PSRAM holds only zeroed runtime tables (the session table, voice
    # buffers): NOLOAD, below the bench diagnostics' final MiB.
    buffers = sections.get(".psram_bss")
    if buffers and buffers[5]:
        assert buffers[1] == 8, ".psram_bss must be NOLOAD"
        assert PSRAM_START <= buffers[3] <= buffers[3] + buffers[5] <= PSRAM_SCRATCH, "PSRAM buffers overlap the scratch area"

    assets = sections[".flash_assets"]
    asset_load = load_address(assets)
    if bootstrap:
        assert APP_BASE <= asset_load < asset_load + assets[5] <= BONDS_START, "bootstrap assets must fit in lower NOR"
    else:
        assert asset_load == BONDS_END, "font load image must use upper NOR"
    stored = loaded = 0
    physical_end = APP_BASE
    for _, offset, vaddr, paddr, size, memory, flags, _ in sorted(segments, key=lambda s: s[3]):
        assert offset + size <= len(data) and size <= memory, "truncated load segment"
        assert APP_BASE <= paddr < paddr + size <= APP_END, "segment exceeds application flash"
        assert paddr + size <= BONDS_START or paddr >= BONDS_END, "segment overlaps Bluetooth storage"
        assert paddr >= physical_end, "overlapping flash load images"
        physical_end = paddr + size
        if APP_BASE <= vaddr < APP_END:
            assert vaddr == paddr, "NOR code must execute in place"
        else:
            assert SRAM_START <= vaddr < vaddr + size <= SRAM_END, "unknown runtime load region"
        stored = max(stored, paddr + size - APP_BASE)
        loaded += size
    if bootstrap:
        assert APP_BASE + stored <= BONDS_START, "bootstrap must fit protocol-1 recovery"
    # Check the Rust bodies, not linker-generated long-call thunks.
    found = set()
    required = ("3pmu5enter", "10bond_flash11erase_block", "10bond_flash12program_slot")
    if bootstrap:
        required += ("11boot_update6nor_op",)
    for name, value, size in functions:
        if not name.startswith(("_R", "_ZN")):
            continue
        for marker in required:
            if marker in name:
                assert SRAM_START <= value < value + size <= SRAM_END, f"{marker} must stay in SRAM"
                found.add(marker)
    if b"OpenWalkieTalkie Rev " in data:
        assert found == set(required), "missing SRAM flash-writing/shutdown routines"
    print(f"Execution layout verified: {loaded} load bytes ({stored} byte image span including padding); code/fonts in NOR, {len(found)} critical routines in SRAM")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf")
    parser.add_argument("--bootstrap", action="store_true", help="check a temporary lower-bank bootloader updater")
    args = parser.parse_args()
    check(args.elf, args.bootstrap)
