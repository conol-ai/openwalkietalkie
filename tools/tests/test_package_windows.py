import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location(
    "package_windows", Path(__file__).resolve().parents[1] / "package-windows.py")
package_windows = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_windows)


def pe(subsystem, machine=0x8664, body=b""):
    """The smallest header `pe_header` reads: MZ, e_lfanew, PE\\0\\0, the COFF
    machine and the optional header's subsystem."""
    data = bytearray(0x80 + 0x60)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<H", data, 0x84, machine)
    struct.pack_into("<H", data, 0x80 + 0x5C, subsystem)
    return bytes(data) + body


class PackageWindowsTests(unittest.TestCase):
    def build(self, deckd, app):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        (directory / "deckd.exe").write_bytes(deckd)
        (directory / "app.exe").write_bytes(app)
        return package_windows.package("1.2.3", directory / "deckd.exe", directory / "app.exe", directory / "out")

    def test_the_zip_holds_bin_the_notes_and_the_licence_under_one_folder(self):
        deckd, app = pe(3, body=b"deckd"), pe(2, body=b"app")
        archive = self.build(deckd, app)
        self.assertEqual(archive.name, "OpenWalkieTalkie-1.2.3-windows-x86_64.zip")
        with zipfile.ZipFile(archive) as zf:
            root = "OpenWalkieTalkie-1.2.3-windows-x86_64/"
            self.assertEqual(sorted(zf.namelist()), [root + name for name in
                             ["LICENSE.txt", "README.txt", "bin/OpenWalkieTalkie.exe", "bin/deckd.exe"]])
            self.assertEqual(zf.read(root + "bin/deckd.exe"), deckd)
            self.assertEqual(zf.read(root + "bin/OpenWalkieTalkie.exe"), app)
            readme = zf.read(root + "README.txt").decode()
            self.assertIn("1.2.3", readme)
            self.assertIn("Run anyway", readme)
            self.assertIn("\r\n", readme, "Notepad-friendly line endings")
        digest, name = (archive.parent / f"{archive.name}.sha256").read_text().split()
        self.assertEqual((digest, name), (hashlib.sha256(archive.read_bytes()).hexdigest(), archive.name))

    def test_rebuilding_gives_the_same_bytes(self):
        first = self.build(pe(3), pe(2)).read_bytes()
        self.assertEqual(self.build(pe(3), pe(2)).read_bytes(), first)

    def test_a_console_app_a_gui_deckd_or_another_machine_is_refused(self):
        for deckd, app, message in [
            (pe(3), pe(3), "GUI"),          # the app would open a console window
            (pe(2), pe(2), "console"),      # deckd is also the command line tool
            (pe(3, machine=0x14C), pe(2), "x86-64"),
            (b"not a program", pe(2), "Windows executable"),
        ]:
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                self.build(deckd, app)


if __name__ == "__main__":
    unittest.main()
