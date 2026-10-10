import unittest
from unittest.mock import AsyncMock, MagicMock, patch
import json
import tempfile
from pathlib import Path
import os
import asyncio

import gemini_auto


class TestGeminiAuto(unittest.TestCase):
    def test_extract_sid_and_sidts_both_present(self):
        cookies = [
            {"name": "__Secure-1PSID", "value": "sid_val_123", "domain": ".google.com"},
            {"name": "__Secure-1PSIDTS", "value": "sidts_val_456", "domain": ".google.com"},
            {"name": "OTHER", "value": "other_val", "domain": ".google.com"},
        ]
        sid, sidts = gemini_auto.extract_sid_and_sidts(cookies)
        self.assertEqual(sid, "sid_val_123")
        self.assertEqual(sidts, "sidts_val_456")

    def test_extract_sid_and_sidts_missing_sidts(self):
        # Simulates the condition during login before redirect completes
        cookies = [
            {"name": "__Secure-1PSID", "value": "sid_val_123", "domain": ".google.com"},
        ]
        sid, sidts = gemini_auto.extract_sid_and_sidts(cookies)
        self.assertEqual(sid, "sid_val_123")
        self.assertIsNone(sidts)

    def test_extract_sid_and_sidts_ignores_other_domains(self):
        # Even if youtube.com has 1PSIDTS, it must not be picked up for Gemini
        cookies = [
            {"name": "__Secure-1PSID", "value": "sid_google", "domain": ".google.com"},
            {"name": "__Secure-1PSIDTS", "value": "sidts_youtube", "domain": ".youtube.com"},
        ]
        sid, sidts = gemini_auto.extract_sid_and_sidts(cookies)
        self.assertEqual(sid, "sid_google")
        self.assertIsNone(sidts)

    def test_extract_sid_and_sidts_ignores_empty_values(self):
        cookies = [
            {"name": "__Secure-1PSID", "value": "", "domain": ".google.com"},
            {"name": "__Secure-1PSIDTS", "value": "   ", "domain": ".google.com"},
        ]
        sid, sidts = gemini_auto.extract_sid_and_sidts(cookies)
        self.assertIsNone(sid)
        self.assertIsNone(sidts)

    def test_load_saved_cookies_valid(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            cookie_path = Path(tmpdir) / "cookies.json"
            cookie_path.write_text(json.dumps({"sid": "test_sid", "sidts": "test_sidts"}))
            with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                res = gemini_auto.load_saved_cookies()
                self.assertEqual(res, ("test_sid", "test_sidts"))

    def test_load_saved_cookies_empty_sidts_rejected(self):
        # Crucial test for the hang bug: sid present but empty sidts must NOT be loaded as valid
        with tempfile.TemporaryDirectory() as tmpdir:
            cookie_path = Path(tmpdir) / "cookies.json"
            cookie_path.write_text(json.dumps({"sid": "test_sid", "sidts": ""}))
            with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                res = gemini_auto.load_saved_cookies()
                self.assertIsNone(res)

    def test_load_saved_cookies_empty_sid_rejected(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            cookie_path = Path(tmpdir) / "cookies.json"
            cookie_path.write_text(json.dumps({"sid": "", "sidts": "test_sidts"}))
            with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                res = gemini_auto.load_saved_cookies()
                self.assertIsNone(res)

    def test_load_saved_cookies_missing_file(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            cookie_path = Path(tmpdir) / "nonexistent.json"
            with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                res = gemini_auto.load_saved_cookies()
                self.assertIsNone(res)

    def test_load_saved_cookies_corrupt_json(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            cookie_path = Path(tmpdir) / "cookies.json"
            cookie_path.write_text("invalid json content")
            with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                res = gemini_auto.load_saved_cookies()
                self.assertIsNone(res)

    def test_perform_login_detects_closed_browser(self):
        async def run_test():
            mock_browser = AsyncMock()
            mock_browser.is_closed.return_value = False
            mock_browser.pages = []  # No pages open (user closed window)
            mock_page = AsyncMock()
            mock_browser.new_page.return_value = mock_page

            mock_playwright = MagicMock()
            mock_playwright.chromium.launch_persistent_context = AsyncMock(return_value=mock_browser)

            with self.assertRaises(RuntimeError) as ctx:
                await gemini_auto.perform_login(mock_playwright, timeout=5)
            self.assertIn("Browser window was closed", str(ctx.exception))

        asyncio.run(run_test())

    def test_perform_login_times_out(self):
        async def run_test():
            mock_browser = AsyncMock()
            mock_browser.is_closed.return_value = False
            mock_browser.pages = [MagicMock()]
            mock_browser.cookies.return_value = []  # Never yields cookies
            mock_page = AsyncMock()
            mock_browser.new_page.return_value = mock_page

            mock_playwright = MagicMock()
            mock_playwright.chromium.launch_persistent_context = AsyncMock(return_value=mock_browser)

            with self.assertRaises(TimeoutError):
                # 0.1 second timeout to trigger immediately
                await gemini_auto.perform_login(mock_playwright, timeout=0.1)

        asyncio.run(run_test())

    def test_perform_login_waits_for_both_cookies_and_saves(self):
        async def run_test():
            with tempfile.TemporaryDirectory() as tmpdir:
                cookie_path = Path(tmpdir) / "cookies.json"
                with patch.object(gemini_auto, "COOKIE_FILE", cookie_path):
                    mock_browser = AsyncMock()
                    mock_browser.is_closed.return_value = False
                    mock_browser.pages = [MagicMock()]
                    
                    # 1st call: only sid (should NOT exit)
                    # 2nd call: both sid and sidts (should exit and save)
                    call_count = 0
                    async def mock_cookies():
                        nonlocal call_count
                        call_count += 1
                        if call_count == 1:
                            return [{"name": "__Secure-1PSID", "value": "sid1", "domain": ".google.com"}]
                        return [
                            {"name": "__Secure-1PSID", "value": "sid1", "domain": ".google.com"},
                            {"name": "__Secure-1PSIDTS", "value": "sidts1", "domain": ".google.com"},
                        ]
                    mock_browser.cookies = mock_cookies
                    mock_page = AsyncMock()
                    mock_browser.new_page.return_value = mock_page

                    mock_playwright = MagicMock()
                    mock_playwright.chromium.launch_persistent_context = AsyncMock(return_value=mock_browser)

                    sid, sidts = await gemini_auto.perform_login(mock_playwright, timeout=5)
                    self.assertEqual(sid, "sid1")
                    self.assertEqual(sidts, "sidts1")
                    self.assertTrue(cookie_path.exists())
                    saved = json.loads(cookie_path.read_text())
                    self.assertEqual(saved["sid"], "sid1")
                    self.assertEqual(saved["sidts"], "sidts1")

        asyncio.run(run_test())


if __name__ == "__main__":
    unittest.main()
