"""Exercise execution-layout checks with small ELF fixtures and unsafe mutations."""
import contextlib
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("layout", Path(__file__).resolve().parents[1] / "check-xip-image.py")
layout = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(layout)


class Image:
    def __init__(self, board="A", psram=False, bootstrap=False):
        self.sections = {}
        self.symbols = {}
        base = layout.APP_BASE
        # `psram` links code at PSRAM, the layout Rev A used before 0.13.
        text = layout.PSRAM_START if psram else base + 0x100
        assets = base + 0x300 if bootstrap else layout.BONDS_END
        self.add(".vector_table", base, base, struct.pack("<16I", 0x2007FC00, base + 0x101, text + 1, *([0] * 13)))
        self.add(".text", text, base + 0x100, bytes(64))
        self.add(".rodata", base + 0x240, base + 0x240, f"OpenWalkieTalkie Rev {board} (fixture)".encode().ljust(64, b"\0"))
        self.add(".flash_assets", assets, assets, bytes(64))
        self.add(".data", layout.SRAM_START, base + 0x400, bytes(16))
        self.add(".psram_bss", layout.PSRAM_START + (0x1000 if psram else 0), None, bytes(64))
        for i, name in enumerate(("3pmu5enter", "10bond_flash11erase_block", "10bond_flash12program_slot") + (("11boot_update6nor_op",) if bootstrap else ())):
            self.symbols["_ZN" + name] = (layout.SRAM_START + i * 4 + 1, 2, 2)

    def add(self, name, addr, load, body, align=32):
        self.sections[name] = dict(addr=addr, load=load, body=body, align=align)

    def bytes(self):
        count = sum(s["load"] is not None for s in self.sections.values())
        data = bytearray(52 + 32 * count)
        data[:7] = b"\x7fELF\x01\x01\x01"
        headers = [bytes(40)]
        names = bytearray(b"\0")
        program = 52
        for name, s in self.sections.items():
            offset = len(data)
            body, addr = s["body"], s["addr"]
            kind = 8 if s["load"] is None else 1
            if kind != 8:
                data += body
                struct.pack_into("<8I", data, program, 1, offset, addr, s["load"], len(body), len(body), 7, s["align"])
                program += 32
            headers.append(struct.pack("<10I", len(names), kind, 2, addr, offset, len(body), 0, 0, s["align"], 0))
            names += name.encode() + b"\0"
        strings = bytearray(b"\0")
        symbols = bytearray(16)
        for name, (value, size, kind) in self.symbols.items():
            symbols += struct.pack("<IIIBBH", len(strings), value, size, kind, 0, 1)
            strings += name.encode() + b"\0"
        for name, body, kind, link, entsize in ((".symtab", symbols, 2, len(headers) + 1, 16),
                                               (".strtab", strings, 3, 0, 0)):
            headers.append(struct.pack("<10I", len(names), kind, 0, 0, len(data), len(body), link, 0, 1, entsize))
            names += name.encode() + b"\0"
            data += body
        index = len(headers)
        name_offset = len(names)
        names += b".shstrtab\0"
        headers.append(struct.pack("<10I", name_offset, 3, 0, 0, len(data), len(names), 0, 0, 1, 0))
        data += names
        struct.pack_into("<II", data, 28, 52, len(data))
        struct.pack_into("<5H", data, 42, 32, count, 40, len(headers), index)
        return data + b"".join(headers)


class ExecutionLayoutTests(unittest.TestCase):
    def check(self, image, bootstrap=False):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.elf"
            path.write_bytes(image.bytes())
            with contextlib.redirect_stdout(io.StringIO()):
                layout.check(path, bootstrap)

    def test_normal_and_bootstrap_board_layouts(self):
        for bootstrap in (False, True):
            for board in ("A", "B"):
                with self.subTest(board=board, bootstrap=bootstrap):
                    self.check(Image(board, bootstrap=bootstrap), bootstrap)

    def test_code_linked_into_psram_is_refused_on_every_board(self):
        for board in ("A", "B"):
            with self.subTest(board=board), self.assertRaises(AssertionError):
                self.check(Image(board, psram=True))

    def test_fonts_must_load_from_their_bank(self):
        for field, value in (("addr", layout.PSRAM_START), ("load", layout.BONDS_START)):
            image = Image()
            image.sections[".flash_assets"][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(AssertionError):
                self.check(image)

    def test_psram_tables_stay_below_the_scratch_area(self):
        image = Image()
        image.sections[".psram_bss"]["addr"] = layout.PSRAM_SCRATCH - 32
        with self.assertRaisesRegex(AssertionError, "scratch"):
            self.check(image)

    def test_sram_flash_writer_cannot_move_to_psram(self):
        image = Image()
        image.symbols["_ZN10bond_flash11erase_block"] = (layout.PSRAM_START + 1, 2, 2)
        with self.assertRaisesRegex(AssertionError, "must stay in SRAM"):
            self.check(image)
