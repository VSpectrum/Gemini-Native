import sys
import json
import asyncio
from pathlib import Path
import io
from gemini_webapi.types.image import WebImage, GeneratedImage
from gemini_webapi.types.video import GeneratedVideo, Video
from gemini_webapi import GeminiClient
from gemini_auto import get_cookies

MEDIA_DIR = Path.home() / ".gemini_local" / "media"
MEDIA_DIR.mkdir(parents=True, exist_ok=True)

async def main():
    log_capture = io.StringIO()
    try:
        from loguru import logger as loguru_logger
        loguru_logger.add(log_capture)
    except ImportError:
        pass
        
    if len(sys.argv) < 2:
        print(json.dumps({"error": "No media json provided"}))
        return

    try:
        media_data = json.loads(sys.argv[1])
        media_type = media_data.get("type")
        
        sid, sidts = await get_cookies()
        client = GeminiClient(sid, sidts)
        await client.init(timeout=15, auto_refresh=False)
        
        item = None
        if media_type == "web_image":
            item = WebImage(**media_data["meta"])
        elif media_type == "generated_image":
            item = GeneratedImage(**media_data["meta"])
            item.client_ref = client
        elif media_type == "video":
            item = Video(**media_data["meta"])
        elif media_type == "generated_video":
            item = GeneratedVideo(**media_data["meta"])
            item.client_ref = client
        else:
            print(json.dumps({"error": f"Unknown media type: {media_type}"}))
            return
            
        saved_path = await item.save(path=str(MEDIA_DIR), verbose=False)
        print(json.dumps({"status": "ok", "path": saved_path}))
    except Exception as e:
        print(json.dumps({"error": str(e)}))

if __name__ == "__main__":
    asyncio.run(main())
