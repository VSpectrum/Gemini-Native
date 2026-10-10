import os
import sys
import json
import asyncio
import io
import re
import subprocess
from pathlib import Path


def _default_browsers_dir() -> Path:
    """Playwright's standard per-user browser cache location."""
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        return Path(base) / "ms-playwright"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "ms-playwright"
    return Path.home() / ".cache" / "ms-playwright"


# When bundled with PyInstaller, Playwright defaults PLAYWRIGHT_BROWSERS_PATH to "0",
# i.e. the temporary _MEI extraction dir that is wiped on every run. Pin it to a
# persistent location (kept outside CACHE_DIR so "Clear Session" doesn't delete it).
# Must be set before Playwright starts its driver.
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", str(_default_browsers_dir()))

from playwright.async_api import async_playwright
from gemini_webapi import GeminiClient

# Cache locations in your home directory
CACHE_DIR = Path.home() / ".gemini_local"
COOKIE_FILE = CACHE_DIR / "cookies.json"
PROFILE_DIR = CACHE_DIR / "chrome_profile"
WEBAPI_CACHE_DIR = CACHE_DIR / "gemini_webapi_cache"

CACHE_DIR.mkdir(exist_ok=True)
WEBAPI_CACHE_DIR.mkdir(exist_ok=True)

# Direct gemini_webapi internal cookie cache into CACHE_DIR so "Clear Session" clears everything
os.environ.setdefault("GEMINI_COOKIE_PATH", str(WEBAPI_CACHE_DIR))


def install_chromium():
    """Download Playwright's Chromium using the bundled driver (no pip/Python needed)."""
    from playwright._impl._driver import compute_driver_executable, get_driver_env

    driver = compute_driver_executable()
    cmd = list(driver) if isinstance(driver, (tuple, list)) else [str(driver)]
    kwargs = {}
    if sys.platform == "win32":
        kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
    # Route installer output to stderr so it never pollutes the JSON on stdout
    subprocess.run(
        cmd + ["install", "chromium"],
        env=get_driver_env(),
        stdout=sys.stderr,
        stderr=sys.stderr,
        check=True,
        **kwargs,
    )

def extract_sid_and_sidts(cookies):
    """Extract valid __Secure-1PSID and __Secure-1PSIDTS from cookies list for .google.com."""
    secure_1psid = None
    secure_1psidts = None
    for cookie in cookies:
        name = cookie.get("name")
        domain = cookie.get("domain", "")
        value = cookie.get("value", "")
        if not value or not isinstance(value, str) or not value.strip():
            continue
        # Ensure the cookie belongs to google.com domain (not youtube.com or unrelated)
        if domain.endswith("google.com"):
            if name == "__Secure-1PSID":
                secure_1psid = value.strip()
            elif name == "__Secure-1PSIDTS":
                secure_1psidts = value.strip()
    return secure_1psid, secure_1psidts

async def perform_login(p, timeout=300):
    browser = await p.chromium.launch_persistent_context(
        user_data_dir=str(PROFILE_DIR),
        headless=False,
        args=["--disable-blink-features=AutomationControlled"]
    )
    
    page = await browser.new_page()
    try:
        await page.goto("https://gemini.google.com", wait_until="domcontentloaded", timeout=45000)
    except Exception as e:
        print(f"Notice: Initial navigation: {e}", file=sys.stderr)
    
    secure_1psid = None
    secure_1psidts = None
    
    print("Waiting for session cookies (__Secure-1PSID and __Secure-1PSIDTS)...", file=sys.stderr)
    start_time = asyncio.get_event_loop().time()
    
    while not (secure_1psid and secure_1psidts):
        # Check if user closed the browser window before completing sign-in
        is_closed_val = False
        if hasattr(browser, "is_closed"):
            check_res = browser.is_closed()
            if asyncio.iscoroutine(check_res):
                is_closed_val = await check_res
            else:
                is_closed_val = bool(check_res)
        
        if is_closed_val or len(browser.pages) == 0:
            raise RuntimeError("Browser window was closed before sign-in completed.")
        
        if asyncio.get_event_loop().time() - start_time > timeout:
            await browser.close()
            raise TimeoutError("Timed out waiting for sign-in cookies (5 minutes exceeded).")
        
        try:
            cookies = await browser.cookies()
            secure_1psid, secure_1psidts = extract_sid_and_sidts(cookies)
        except Exception as e:
            if "Target closed" in str(e) or "browser has been closed" in str(e):
                raise RuntimeError("Browser window was closed before sign-in completed.") from e
            raise
        
        if not (secure_1psid and secure_1psidts):
            await asyncio.sleep(1)
    
    # Allow a brief moment for redirection and cookie settlement
    await asyncio.sleep(1.0)
    try:
        cookies = await browser.cookies()
        sid2, sidts2 = extract_sid_and_sidts(cookies)
        if sid2:
            secure_1psid = sid2
        if sidts2:
            secure_1psidts = sidts2
    except Exception:
        pass
    
    await browser.close()
    
    def save_cookies():
        with open(COOKIE_FILE, "w") as f:
            json.dump({"sid": secure_1psid, "sidts": secure_1psidts}, f)

    await asyncio.to_thread(save_cookies)
        
    return secure_1psid, secure_1psidts

async def extract_gemini_cookies():
    # Route prints to stderr so they don't break Rust's JSON parser
    print("Launching browser to capture fresh cookies...", file=sys.stderr)
    async with async_playwright() as p:
        try:
            return await perform_login(p)
        except Exception as e:
            if "Executable doesn't exist at" in str(e) or "Looks like Playwright" in str(e):
                print(
                    f"Chromium not found. Installing to {os.environ['PLAYWRIGHT_BROWSERS_PATH']}...",
                    file=sys.stderr,
                )
                await asyncio.to_thread(install_chromium)
                # Retry after installation
                return await perform_login(p)
            raise

def load_saved_cookies():
    """Read saved cookies and ensure BOTH sid and sidts are non-empty strings."""
    try:
        with open(COOKIE_FILE, "r") as f:
            data = json.load(f)
            sid = data.get("sid")
            sidts = data.get("sidts")
            if (
                isinstance(sid, str)
                and sid.strip()
                and isinstance(sidts, str)
                and sidts.strip()
            ):
                return sid.strip(), sidts.strip()
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        pass
    return None

async def get_cookies(force_refresh=False):
    if not force_refresh:
        result = await asyncio.to_thread(load_saved_cookies)
        if result:
            return result
    else:
        # If forcing refresh, also delete stale cookie file and webapi cache
        def cleanup():
            try:
                COOKIE_FILE.unlink(missing_ok=True)
            except OSError:
                pass
            if WEBAPI_CACHE_DIR.exists():
                import shutil
                shutil.rmtree(WEBAPI_CACHE_DIR, ignore_errors=True)
                WEBAPI_CACHE_DIR.mkdir(exist_ok=True)
        await asyncio.to_thread(cleanup)
    
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
        await asyncio.wait_for(client.init(timeout=15, auto_refresh=True), timeout=30)
    except Exception as e:
        # Fails silently in stdout, triggers Playwright visibly via stderr
        print(f"Notice: Initial authentication failed ({e}), refreshing session...", file=sys.stderr)
        try:
            sid, sidts = await get_cookies(force_refresh=True)
            client = GeminiClient(sid, sidts)
            await asyncio.wait_for(client.init(timeout=15, auto_refresh=True), timeout=30)
        except Exception as retry_err:
            print(json.dumps({
                "text": f"Error: Failed to authenticate with Gemini ({retry_err}). Please check your connection or try 'Clear Session'.",
                "quota": "Error: Auth failed",
                "abuse": "Unknown",
                "media": []
            }))
            return
        
    # 3. Generate Content
    kwargs = {}
    if model_name:
        kwargs["model"] = model_name
        
    async def run_prompt():
        if metadata:
            from gemini_webapi.client import ChatSession
            chat = ChatSession(client, metadata=metadata)
            chat_kwargs = {k: v for k, v in kwargs.items() if k != "model"}
            return await chat.send_message(prompt, **chat_kwargs)
        else:
            return await client.generate_content(prompt, **kwargs)

    try:
        response = await asyncio.wait_for(run_prompt(), timeout=120)
    except asyncio.TimeoutError:
        print(json.dumps({
            "text": "Error: Request to Gemini timed out after 120 seconds. The session cookies may have expired or the connection was dropped. Please try clicking 'Clear Session' and signing in again.",
            "quota": "Error: Timeout",
            "abuse": "Unknown",
            "media": []
        }))
        return
    except Exception as e:
        print(json.dumps({
            "text": f"Error: Failed to generate response: {e}",
            "quota": "Error",
            "abuse": "Unknown",
            "media": []
        }))
        return
    
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
    
    print("\n" + json.dumps(output))

if __name__ == "__main__":
    asyncio.run(main())