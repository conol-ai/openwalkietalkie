import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("release", Path(__file__).resolve().parents[1] / "release.py")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def image(board="A"):
    return struct.pack("<II", 0x20040000, release.APP_BASE + 9) + b"\0" * 12 + f"OpenWalkieTalkie Rev {board} (test)".encode()


def bootstrap(board="A"):
    return image(board) + f"OpenWalkieTalkie Bootstrap 1 Rev {board}".encode()


def recovery(board="A"):
    return struct.pack("<II", 0x20042000, 0x20020009) + f"OpenWalkieTalkie Recovery 2 Rev {board}".encode()


def elf(payload, vaddr=0x60000000):
    data = bytearray(84)
    data[:7] = b"\x7fELF\x01\x01\x01"
    struct.pack_into("<I", data, 28, 52)
    struct.pack_into("<HH", data, 42, 32, 1)
    struct.pack_into("<8I", data, 52, 1, 84, vaddr, release.APP_BASE, len(payload), len(payload), 5, 4)
    return data + payload


class ReleaseSiteTests(unittest.TestCase):
    def test_release_requires_a_discoverable_post_and_current_downloads(self):
        url = "https://openwalkietalkie.org/blog/new-release/"
        release_url = f"https://github.com/{release.REPOSITORY}/releases"
        files = {
            "blog/feed.xml": f'''<feed xmlns="http://www.w3.org/2005/Atom"><entry>
                <title>OpenWalkieTalkie v1.2.3: New release</title>
                <link href="{url}" /></entry></feed>''',
            "blog/new-release/index.html": f'<a href="{release_url}/tag/v1.2.3">Release</a>',
            "blog/index.html": '<a href="/blog/new-release/">Read the release</a>',
            "sitemap.xml": f'''<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                <url><loc>{url}</loc></url></urlset>''',
            "index.html": '\n'.join(
                f'<a href="{release_url}/download/v1.2.3/OpenWalkieTalkie-1.2.3-macos-{arch}.dmg">Mac</a>'
                for arch in ("aarch64", "x86_64")),
        }
        # Reuse the published assets so the release fixture checks their contract too.
        for name in ["docs/index.html", "docs/native-companions/index.html",
                     "docs/deckd-api.md", "examples/native_companion.py"]:
            files[name] = (release.ROOT / "site/public" / name).read_text()
        files["index.html"] += '<a href="/docs/">Docs</a>'
        files["sitemap.xml"] = files["sitemap.xml"].replace("</urlset>",
            '<url><loc>https://openwalkietalkie.org/docs/</loc></url>'
            '<url><loc>https://openwalkietalkie.org/docs/native-companions/</loc></url></urlset>')
        with tempfile.TemporaryDirectory() as directory:
            public = Path(directory)
            for name, content in files.items():
                path = public / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            release.validate_site("v1.2.3", public)

            # Reproduce each way a release can ship while its website stays stale.
            cases = [
                ("blog/feed.xml", files["blog/feed.xml"].replace("v1.2.3:", "v1.2.2:"), "feed"),
                ("blog/feed.xml", files["blog/feed.xml"].replace(url, "https://example.com/post/"), "URL"),
                ("blog/new-release/index.html", None, "blog post"),
                ("blog/new-release/index.html", files["blog/new-release/index.html"].replace("v1.2.3", "v1.2.2"), "blog post"),
                ("blog/index.html", "<p>No posts yet</p>", "index"),
                ("sitemap.xml", files["sitemap.xml"].replace(url, "https://openwalkietalkie.org/"), "sitemap"),
            ]
            cases.extend([
                ("docs/native-companions/index.html", None, "documentation missing"),
                ("docs/native-companions/index.html", files["docs/native-companions/index.html"].replace("session.settings_requested", "wrong.event"), "guide must contain"),
                ("docs/index.html", "<p>No guide</p>", "docs index"),
                ("index.html", files["index.html"].replace('href="/docs/"', 'href="/"'), "home must link"),
                ("sitemap.xml", files["sitemap.xml"].replace("https://openwalkietalkie.org/docs/native-companions/", "https://example.com/"), "sitemap"),
                ("examples/native_companion.py", "# stale example", "download must match"),
                ("docs/deckd-api.md", "# stale reference", "download must match"),
            ])
            for arch in ("aarch64", "x86_64"):
                cases.append(("index.html", files["index.html"].replace(f"1.2.3-macos-{arch}", f"1.2.2-macos-{arch}"), arch))
            for name, content, message in cases:
                with self.subTest(file=name, message=message):
                    path = public / name
                    if content is None:
                        path.unlink()
                    else:
                        path.write_text(content)
                    try:
                        with self.assertRaisesRegex(ValueError, message):
                            release.validate_site("v1.2.3", public)
                    finally:
                        path.write_text(files[name])

            # Earlier releases do not retroactively require the companion pages.
            for name, content in files.items():
                if name.startswith(("docs/", "examples/")):
                    (public / name).unlink()
                else:
                    (public / name).write_text(content.replace("1.2.3", "0.10.0"))
            release.validate_site("v0.10.0", public)


class ReleaseTests(unittest.TestCase):
    def test_real_application_extraction_preserves_bytes(self):
        for board in ("a", "b"):
            for address in (release.APP_BASE, 0x60000000):
                payload = image(board.upper())
                self.assertEqual(release.image_from_elf(elf(payload, address), f"hw-{board}"), payload)

    def test_other_board_is_refused(self):
        with self.assertRaisesRegex(ValueError, "board"):
            release.image_from_elf(elf(image()), "hw-b")

    def test_truncated_elf_and_bad_vector_are_refused(self):
        with self.assertRaises(ValueError):
            release.image_from_elf(elf(image())[:-5], "hw-a")
        payload = bytearray(image())
        struct.pack_into("<I", payload, 4, release.APP_BASE + 100001)
        with self.assertRaisesRegex(ValueError, "vector"):
            release.image_from_elf(elf(payload), "hw-a")

    def test_bootloader_unmapped_and_non_executable_segments_cannot_be_released(self):
        for offset, value in [(64, 0x12010000), (60, 0x20000000), (76, 4)]:
            data = elf(image())
            struct.pack_into("<I", data, offset, value)
            with self.assertRaises(ValueError):
                release.image_from_elf(data, "hw-a")

    def test_xip_does_not_allow_unknown_or_ambiguous_board_images(self):
        for payload in [image().replace(b"Rev A", b"Other"), image() + image("B")]:
            with self.assertRaisesRegex(ValueError, "board"):
                release.image_from_elf(elf(payload, release.APP_BASE), "hw-a")

    def test_manifest_binds_both_boards_to_release_assets(self):
        app, firmware = release.versions()
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            for board in ("a", "b"):
                (directory / f"OpenWalkieTalkie-{firmware}-hw-{board}.bin").write_bytes(image(board.upper()))
                (directory / f"OpenWalkieTalkie-{firmware}-bootstrap-hw-{board}.bin").write_bytes(bootstrap(board.upper()))
                (directory / f"OpenWalkieTalkie-{firmware}-recovery-hw-{board}.bin").write_bytes(recovery(board.upper()))
            for arch in ("aarch64", "x86_64"):
                (directory / f"OpenWalkieTalkie-{app}-macos-{arch}.dmg").write_bytes(b"fixture")
            manifest = release.manifest(directory, f"v{app}")
            for board, asset in manifest["firmware"]["variants"].items():
                self.assertIn(board, asset["name"])
                self.assertEqual(len(asset["sha256"]), 64)
                self.assertTrue(asset["url"].startswith(f"https://github.com/conol-ai/openwalkietalkie/releases/download/v{app}/"))
                migration = manifest["firmware"]["migrations"][board]
                self.assertIn(f"bootstrap-{board}", migration["bootstrap"]["name"])
                self.assertIn(f"recovery-{board}", migration["bootloader"]["name"])
            with self.assertRaises(ValueError):
                release.manifest(directory, "v999.0.0")
            (directory / f"OpenWalkieTalkie-{firmware}-recovery-hw-b.bin").unlink()
            with self.assertRaises(FileNotFoundError):
                release.manifest(directory, f"v{app}")

    def test_bootstrap_cannot_be_published_as_normal_application(self):
        for board in ("a", "b"):
            payload = bootstrap(board.upper())
            release.validate_image(payload, f"hw-{board}", bootstrap=True)
            with self.assertRaisesRegex(ValueError, "normal firmware"):
                release.validate_image(payload, f"hw-{board}")
            with self.assertRaises(ValueError):
                release.validate_image(image(board.upper()), f"hw-{board}", bootstrap=True)
            oversized = payload + b"\xff" * (release.BONDS_START - release.APP_BASE)
            with self.assertRaises(ValueError):
                release.validate_image(oversized, f"hw-{board}", bootstrap=True)

    def test_recovery_requires_sram_vectors_and_exact_board_marker(self):
        for board in ("a", "b"):
            release.validate_recovery(recovery(board.upper()), f"hw-{board}")
            with self.assertRaises(ValueError):
                release.validate_recovery(image(board.upper()), f"hw-{board}")
            with self.assertRaises(ValueError):
                release.validate_recovery(recovery("B" if board == "a" else "A"), f"hw-{board}")
            with self.assertRaises(ValueError):
                release.validate_recovery(recovery("A") + recovery("B"), f"hw-{board}")
