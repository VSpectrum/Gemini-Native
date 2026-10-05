import sys
import json
import os
import asyncio
import logging
import io
import re
from pathlib import Path
from playwright.async_api import async_playwright
from gemini_webapi import GeminiClient

# Cache locations in your home directory
CACHE_DIR = Path.home() / ".gemini_local"
COOKIE_FILE = CACHE_DIR / "cookies.json"
PROFILE_DIR = CACHE_DIR / "chrome_profile"

CACHE_DIR.mkdir(exist_ok=True)

async def extract_gemini_cookies():
    # Route prints to stderr so they don't break Rust's JSON parser
    print("Launching browser to capture fresh cookies...", file=sys.stderr)
    async with async_playwright() as p:
        browser = await p.chromium.launch_persistent_context(
            user_data_dir=str(PROFILE_DIR),
            headless=False,
            args=["--disable-blink-features=AutomationControlled"]
        )
        
        page = await browser.new_page()
        await page.goto("https://gemini.google.com")
        
        secure_1psid = None
        secure_1psidts = ""
        
        print("Waiting for session cookies...", file=sys.stderr)
        while not secure_1psid:
            cookies = await browser.cookies()
            for cookie in cookies:
                if cookie['name'] == '__Secure-1PSID':
                    secure_1psid = cookie['value']
                elif cookie['name'] == '__Secure-1PSIDTS':
                    secure_1psidts = cookie['value']
            
            if not secure_1psid:
                await asyncio.sleep(1)
        
        await browser.close()
        
        with open(COOKIE_FILE, "w") as f:
            json.dump({"sid": secure_1psid, "sidts": secure_1psidts}, f)
            
        return secure_1psid, secure_1psidts

async def get_cookies(force_refresh=False):
    if not force_refresh and COOKIE_FILE.exists():
        with open(COOKIE_FILE, "r") as f:
            data = json.load(f)
            return data.get("sid"), data.get("sidts", "")
    
    return await extract_gemini_cookies()

async def main():
    if len(sys.argv) < 2:
        print(json.dumps({"text": "Error: No prompt provided", "quota": "", "abuse": ""}))
        return
        
    prompt = sys.argv[1]
    model_name = sys.argv[2] if len(sys.argv) > 2 else None
    
    metadata = None
    if len(sys.argv) > 3 and sys.argv[3]:
        try:
            metadata = json.loads(sys.argv[3])
        except Exception:
            pass
    
    # 1. Intercept the logger in memory so it doesn't print to stdout
    log_capture = io.StringIO()
    try:
        from loguru import logger as loguru_logger
        loguru_logger.add(log_capture)
    except ImportError:
        pass
    
    # 2. Authenticate
    sid, sidts = await get_cookies()
    client = GeminiClient(sid, sidts)
    
    try:
        await client.init(timeout=15, auto_refresh=True)
    except Exception:
        # Fails silently in stdout, triggers Playwright visibly via stderr
        sid, sidts = await get_cookies(force_refresh=True)
        client = GeminiClient(sid, sidts)
        await client.init(timeout=15, auto_refresh=True)
        
    # 3. Generate Content
    kwargs = {}
    if model_name:
        kwargs["model"] = model_name
        
    if metadata:
        from gemini_webapi.client import ChatSession
        chat = ChatSession(client, metadata=metadata)
        # ChatSession manages the model internally, so don't pass it again
        chat_kwargs = {k: v for k, v in kwargs.items() if k != "model"}
        response = await chat.send_message(prompt, **chat_kwargs)
    else:
        response = await client.generate_content(prompt, **kwargs)
    
    # 4. Extract Quota and Abuse Status
    logs = log_capture.getvalue()
    quota_matches = re.findall(r'Account quota updated:\s*(.*)', logs)
    quota_summary = " | ".join(quota_matches) if quota_matches else "Quota: Unknown"
    
    abuse_matches = re.findall(r'Account abuse status:\s*(.*)', logs)
    abuse_summary = " | ".join(abuse_matches) if abuse_matches else "Abuse Status: Unknown"
    
    # 5. Extract Media
    media_items = []
    if hasattr(response, "candidates") and response.candidates:
        candidate = response.candidates[0]
        
        for img in candidate.web_images:
            media_items.append({
                "type": "web_image",
                "meta": {
                    "url": img.url,
                    "title": img.title,
                    "alt": img.alt
                }
            })
            
        for img in candidate.generated_images:
            media_items.append({
                "type": "generated_image",
                "meta": {
                    "url": img.url,
                    "title": img.title,
                    "alt": img.alt,
                    "cid": img.cid,
                    "rid": img.rid,
                    "rcid": img.rcid,
                    "image_id": img.image_id
                }
            })
            
        for vid in candidate.generated_videos:
            media_items.append({
                "type": "generated_video",
                "meta": {
                    "url": vid.url,
                    "title": vid.title,
                    "thumbnail": vid.thumbnail,
                    "cid": vid.cid,
                    "rid": vid.rid,
                    "rcid": vid.rcid
                }
            })
    
    # 6. Output strict JSON to stdout for Rust to parse
    output = {
        "text": response.text,
        "metadata": response.metadata,
        "quota": quota_summary,
        "abuse": abuse_summary,
        "media": media_items
    }
    
    print(json.dumps(output), end="")

if __name__ == "__main__":
    asyncio.run(main())