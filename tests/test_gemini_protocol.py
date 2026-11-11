import json
import unittest
from pathlib import Path

import gemini_protocol as protocol


INIT_HTML = """
<html><script>
"SNlM0e":"token-123",
"cfb2h":"boq_assistant-bard-web-server_20260101.00_p0",
"FdrFJe":"session-abc",
"TuX5cc":"en"
</script></html>
"""


def candidate_frame(text, cid="cid-1", rid="rid-1"):
    inner = [None, [cid, rid], None, None, [["rcid-1", [text]]]]
    return [["wrb.fr", None, json.dumps(inner, separators=(",", ":"))]]


class TestProtocolFixtures(unittest.TestCase):
    def test_init_page_extracts_session_tokens(self):
        parsed = protocol.parse_init_page(INIT_HTML)
        self.assertEqual(parsed["access_token"], "token-123")
        self.assertEqual(parsed["build_label"], "boq_assistant-bard-web-server_20260101.00_p0")
        self.assertEqual(parsed["session_id"], "session-abc")
        self.assertEqual(parsed["language"], "en")

    def test_init_page_rejects_a_signed_out_page(self):
        with self.assertRaises(protocol.ProtocolError):
            protocol.parse_init_page("<html>sign in</html>")

    def test_frame_parser_reads_a_length_prefixed_reply(self):
        payload = candidate_frame("pong")
        raw = ")]}'\n" + protocol.encode_frame(payload)
        frames = protocol.FrameParser().feed(raw)
        text, metadata, media = protocol.parse_generate_frames(frames)
        self.assertEqual(text, "pong")
        self.assertEqual(metadata[:2], ["cid-1", "rid-1"])
        self.assertEqual(media, [])

    def test_frame_parser_reassembles_a_split_chunk(self):
        raw = ")]}'\n" + protocol.encode_frame(candidate_frame("hello there"))
        parser = protocol.FrameParser()
        first = parser.feed(raw[:12])
        second = parser.feed(raw[12:])
        text, _, _ = protocol.parse_generate_frames(first + second)
        self.assertEqual(text, "hello there")

    def test_generate_body_carries_the_prompt_and_prior_chat(self):
        model = protocol.ModelChoice("abc123", "gemini-flash", 1, 12, 3)
        form, headers = protocol.build_generate_body(
            "say pong", "token-123", "en", ["cid-1", "rid-1"], model, "SESSION"
        )
        outer = json.loads(form["f.req"])
        inner = json.loads(outer[1])
        self.assertEqual(form["at"], "token-123")
        self.assertEqual(inner[0][0], "say pong")
        self.assertEqual(inner[2][0], "cid-1")
        self.assertEqual(inner[2][1], "rid-1")
        self.assertEqual(len(inner), 81)
        self.assertEqual(inner[7], 1)
        self.assertIn("abc123", headers[protocol.MODEL_HEADER])

    def test_unknown_model_is_rejected_when_the_account_listed_others(self):
        models = [protocol.ModelChoice("abc", "gemini-flash", 1, 12, 1, {"gemini-flash"})]
        with self.assertRaises(protocol.ProtocolError):
            protocol._resolve_model(models, "gemini-does-not-exist")

    def test_short_model_name_matches_a_versioned_discovery_name(self):
        models = [protocol.ModelChoice("abc", "gemini-3-flash", 1, 12, 1, {"gemini-3-flash"})]
        chosen = protocol._resolve_model(models, "gemini-flash")
        self.assertEqual(chosen.model_name, "gemini-3-flash")

    def test_generated_image_url_is_kept_for_download(self):
        image = [
            [None, None, None, [None, None, "a cat", "https://example.test/cat.png"]],
            ["image-1"],
        ]
        rich = [None] * 8
        rich[7] = [[image]]
        candidate = ["rcid-9", ["a cat"], None, None, None, None, None, None, None, None, None, None, rich]
        inner = [None, ["cid", "rid"], None, None, [candidate]]
        frames = [["wrb.fr", None, json.dumps(inner)]]
        text, _, media = protocol.parse_generate_frames(frames)
        self.assertEqual(text, "a cat")
        self.assertEqual(media[0]["type"], "generated_image")
        self.assertEqual(media[0]["meta"]["url"], "https://example.test/cat.png")
        self.assertEqual(media[0]["meta"]["cid"], "cid")


class TestLiveProtocol(unittest.TestCase):
    def test_saved_session_can_generate(self):
        cookies = Path.home() / ".gemini_local" / "cookies.json"
        if not cookies.exists():
            self.skipTest("no saved Gemini cookies")
        try:
            import curl_cffi  # noqa: F401
        except ImportError:
            self.skipTest("curl_cffi is not installed")

        import asyncio

        saved = json.loads(cookies.read_text())
        sid, sidts = saved.get("sid"), saved.get("sidts")
        if not sid or not sidts:
            self.skipTest("saved cookies are incomplete")

        async def run():
            session = protocol.GeminiSession(sid, sidts)
            try:
                await session.open()
                return await session.generate("Reply with exactly the word pong and nothing else.")
            finally:
                await session.close()

        reply = asyncio.run(run())
        self.assertTrue(reply.text.strip())
        self.assertIn("pong", reply.text.lower())
