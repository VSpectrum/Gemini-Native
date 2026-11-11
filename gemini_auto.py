import os
import sys
import json
import asyncio
import shutil
import threading
from pathlib import Path

from browser_session import collect_session_cookies, extract_sid_and_sidts

# Cache locations in your home directory.
# The browser profile lives here too, so "Clear Session" removes it.
CACHE_DIR = Path.home() / ".gemini_local"
COOKIE_FILE = CACHE_DIR / "cookies.json"
PROFILE_DIR = CACHE_DIR / "browser_profile"
WEBAPI_CACHE_DIR = CACHE_DIR / "gemini_webapi_cache"

CACHE_DIR.mkdir(exist_ok=True)
WEBAPI_CACHE_DIR.mkdir(exist_ok=True)

# Older builds cached rotated cookies here. Clear Session still removes the directory.
os.environ.setdefault("GEMINI_COOKIE_PATH", str(WEBAPI_CACHE_DIR))


def ensure_curl_cffi():
    """Development runs use PATH python3, which does not have curl_cffi. The repo venv does."""
    try:
        import curl_cffi  # noqa: F401
    except ImportError:
        scripts = "Scripts" if sys.platform == "win32" else "bin"
        binary = "python.exe" if sys.platform == "win32" else "python"
        venv_python = Path(__file__).resolve().parent / "venv" / scripts / binary
        if venv_python.is_file() and Path(sys.executable).resolve() != venv_python.resolve():
            os.execv(str(venv_python), [str(venv_python), *sys.argv])
        raise


def load_saved_cookies():
    """Read saved cookies and ensure BOTH sid and sidts are non-empty strings."""
    try:
        with open(COOKIE_FILE, "r") as handle:
            data = json.load(handle)
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


def cookies_from_env():
    sid = os.environ.get("GEMINI_PSID", "").strip()
    sidts = os.environ.get("GEMINI_PSIDTS", "").strip()
    if sid and sidts:
        return sid, sidts
    return None


def _save_cookies(sid, sidts):
    COOKIE_FILE.write_text(json.dumps({"sid": sid, "sidts": sidts}))


def _clear_stale_session():
    try:
        COOKIE_FILE.unlink(missing_ok=True)
    except OSError:
        pass
    if WEBAPI_CACHE_DIR.exists():
        shutil.rmtree(WEBAPI_CACHE_DIR, ignore_errors=True)
        WEBAPI_CACHE_DIR.mkdir(exist_ok=True)
    # Drop the sign-in profile too, or the next launch would reuse the cookies that just failed.
    if PROFILE_DIR.exists():
        shutil.rmtree(PROFILE_DIR, ignore_errors=True)


async def get_cookies(force_refresh=False):
    if not force_refresh:
        saved = await asyncio.to_thread(load_saved_cookies)
        if saved:
            return saved
        from_env = cookies_from_env()
        if from_env:
            return from_env
    else:
        await asyncio.to_thread(_clear_stale_session)

    print("Opening a browser to sign in to Gemini...", file=sys.stderr)
    sid, sidts = await asyncio.to_thread(collect_session_cookies, PROFILE_DIR)
    await asyncio.to_thread(_save_cookies, sid, sidts)
    return sid, sidts


async def _close(client):
    if client is not None:
        await client.close()


async def _build_client(force_refresh):
    from gemini_protocol import GeminiSession

    sid, sidts = await get_cookies(force_refresh=force_refresh)
    session = GeminiSession(sid, sidts)
    await asyncio.wait_for(session.open(), timeout=30)
    return session


async def _client_or_refresh():
    try:
        return await _build_client(False)
    except Exception as exc:
        print(f"Notice: Initial authentication failed ({exc}), refreshing session...", file=sys.stderr)
        return await _build_client(True)


async def _generate(client, prompt, model_name, metadata):
    return await client.generate(prompt, model_name, metadata)


async def _run_prompt(client, prompt, model_name, metadata):
    try:
        response = await asyncio.wait_for(
            _generate(client, prompt, model_name, metadata),
            timeout=120,
        )
    except asyncio.TimeoutError:
        return {
            "text": "Error: Request to Gemini timed out after 120 seconds. The session cookies may have expired or the connection was dropped. Please try clicking 'Clear Session' and signing in again.",
            "quota": "Error: Timeout",
            "abuse": "Unknown",
            "media": [],
        }
    except Exception as exc:
        return {
            "text": f"Error: Failed to generate response: {exc}",
            "quota": "Error",
            "abuse": "Unknown",
            "media": [],
        }

    return {
        "text": response.text,
        "metadata": response.metadata,
        "quota": response.quota,
        "abuse": response.abuse,
        "media": response.media,
    }


def _emit(payload):
    # "@@" marks a protocol line so library noise on stdout cannot desync the UI.
    sys.stdout.write("@@" + json.dumps(payload) + "\n")
    sys.stdout.flush()


def _error_payload(text, quota):
    return {"text": text, "quota": quota, "abuse": "Unknown", "media": []}


async def serve():
    """Answer newline-delimited JSON requests on stdin. The Gemini client stays warm."""
    incoming = asyncio.Queue()
    loop = asyncio.get_running_loop()

    def read_stdin():
        try:
            for line in sys.stdin:
                loop.call_soon_threadsafe(incoming.put_nowait, line)
        finally:
            loop.call_soon_threadsafe(incoming.put_nowait, None)

    threading.Thread(target=read_stdin, name="gemini-stdin", daemon=True).start()
    client = None
    try:
        client = await _serve_loop(incoming)
    finally:
        await _close(client)


async def _serve_loop(incoming):
    client = None
    while True:
        line = await incoming.get()
        if line is None:
            return client
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            _emit(_error_payload(f"Error: bad request: {exc}", "Error"))
            continue

        prompt = request.get("prompt") or ""
        model_name = request.get("model") or None
        metadata = request.get("metadata")
        try:
            if client is None:
                try:
                    client = await _client_or_refresh()
                except Exception as exc:
                    _emit(_error_payload(
                        f"Error: Failed to authenticate with Gemini ({exc}). Please check your connection or try 'Clear Session'.",
                        "Error: Auth failed",
                    ))
                    client = None
                    continue
            output = await _run_prompt(client, prompt, model_name, metadata)
            # A failed reply used to exit the process, so the next send started clean.
            # Drop the client and let the next request sign in again. Successes stay warm.
            if str(output.get("text", "")).startswith("Error:"):
                await _close(client)
                client = None
            _emit(output)
        except Exception as exc:
            await _close(client)
            client = None
            _emit(_error_payload(f"Error: Failed to generate response: {exc}", "Error"))


async def main():
    ensure_curl_cffi()
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        await serve()
        return

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

    client = None
    try:
        client = await _client_or_refresh()
    except Exception as exc:
        print(json.dumps(_error_payload(
            f"Error: Failed to authenticate with Gemini ({exc}). Please check your connection or try 'Clear Session'.",
            "Error: Auth failed",
        )))
        return

    try:
        print("\n" + json.dumps(await _run_prompt(client, prompt, model_name, metadata)))
    finally:
        await _close(client)


if __name__ == "__main__":
    asyncio.run(main())
