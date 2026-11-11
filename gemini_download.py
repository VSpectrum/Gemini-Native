import sys
import json
import asyncio
from pathlib import Path

from gemini_auto import ensure_curl_cffi, get_cookies
from gemini_protocol import GeminiSession

MEDIA_DIR = Path.home() / ".gemini_local" / "media"


async def main():
    ensure_curl_cffi()
    if len(sys.argv) < 2:
        print(json.dumps({"error": "No media json provided"}))
        return

    try:
        media_data = json.loads(sys.argv[1])
        url = (media_data.get("meta") or {}).get("url")
        if not isinstance(url, str) or not url.strip():
            print(json.dumps({"error": "No media url provided"}))
            return
        sid, sidts = await get_cookies()
        saved_path = await GeminiSession(sid, sidts).save_url(url, MEDIA_DIR)
        print(json.dumps({"status": "ok", "path": saved_path}))
    except Exception as exc:
        print(json.dumps({"error": str(exc)}))


if __name__ == "__main__":
    asyncio.run(main())
