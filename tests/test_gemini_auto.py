import unittest
from unittest.mock import patch
import json
import tempfile
from pathlib import Path
import os

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

    def test_cookies_from_env(self):
        with patch.dict(os.environ, {"GEMINI_PSID": " env_sid ", "GEMINI_PSIDTS": "env_sidts"}):
            self.assertEqual(gemini_auto.cookies_from_env(), ("env_sid", "env_sidts"))

    def test_cookies_from_env_requires_both(self):
        with patch.dict(os.environ, {"GEMINI_PSID": "only", "GEMINI_PSIDTS": ""}, clear=False):
            os.environ.pop("GEMINI_PSIDTS", None)
            self.assertIsNone(gemini_auto.cookies_from_env())


if __name__ == "__main__":
    unittest.main()
