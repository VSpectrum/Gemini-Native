"""Minimal Gemini web client.

This speaks the same unofficial endpoints the app used through gemini_webapi:
sign-in cookies, the SNlM0e token from the Gemini page, and one StreamGenerate
request. Models, gems, file upload, and deep research are left out.
"""

import asyncio
import json
import random
import re
import uuid
from dataclasses import dataclass, field
from pathlib import Path

INIT_URL = "https://gemini.google.com/app"
GENERATE_URL = (
    "https://gemini.google.com/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate"
)
BATCH_URL = "https://gemini.google.com/_/BardChatUi/data/batchexecute"
USER_STATUS_RPC = "otAQ7b"
QUOTA_RPC = "qpEbW"
FLASH_QUOTA_PAYLOAD = "[[[1,11],[2,11],[6,11]]]"
MODEL_HEADER = "x-goog-ext-525001261-jspb"
BROWSER = "chrome145"

_ACCESS_TOKEN_RE = re.compile(r'"SNlM0e":\s*"(.*?)"')
_BUILD_LABEL_RE = re.compile(r'"cfb2h":\s*"(.*?)"')
_SESSION_ID_RE = re.compile(r'"FdrFJe":\s*"(.*?)"')
_LANGUAGE_RE = re.compile(r'"TuX5cc":\s*"(.*?)"')
_LENGTH_RE = re.compile(r"(\d+)\n")
_ARTIFACTS_RE = re.compile(r"https?://googleusercontent\.com/(?:\w+/)+\d+\n*")
_CARD_RE = re.compile(r"^https?://googleusercontent\.com/card_content/\d+")

_PAGE_HEADERS = {
    "Content-Type": "application/x-www-form-urlencoded;charset=utf-8",
    "Origin": "https://gemini.google.com",
    "Referer": "https://gemini.google.com/",
}
_DEFAULT_METADATA = ["", "", "", None, None, None, None, None, None, ""]


class ProtocolError(RuntimeError):
    pass


@dataclass
class ModelChoice:
    model_id: str
    model_name: str
    capacity: int
    capacity_field: int
    model_number: int
    aliases: set[str] = field(default_factory=set)

    def header_value(self) -> str:
        tail = f"null,{self.capacity}" if self.capacity_field == 13 else str(self.capacity)
        return (
            f'[1,null,null,null,"{self.model_id}",null,null,0,[4,5,6,8],null,null,{tail},'
            f" null,null,{self.model_number}]"
        )


@dataclass
class Reply:
    text: str
    metadata: list
    media: list
    quota: str
    abuse: str


def _nested(data, path, default=None):
    current = data
    for key in path:
        if isinstance(current, list) and isinstance(key, int) and -len(current) <= key < len(current):
            current = current[key]
        elif isinstance(current, dict) and isinstance(key, str) and key in current:
            current = current[key]
        else:
            return default
    return default if current is None else current


def _field(container, index, default=None):
    """Read a JSPB field from its positional slot or the sparse dict at the end."""
    if not isinstance(container, list) or not container:
        return default
    value = container[index] if 0 <= index < len(container) else None
    if value in (None, [], {}) or isinstance(value, dict):
        bundle = container[-1] if isinstance(container[-1], dict) else None
        value = bundle.get(str(index + 1)) if bundle else None
    return default if value in (None, [], {}) else value


def _utf16_units(text: str) -> int:
    return len(text.encode("utf-16-le")) // 2


class FrameParser:
    """Google's length-prefixed frames. The length counts UTF-16 code units and
    includes the newline that separates the length from the JSON."""

    def __init__(self):
        self.buffer = ""
        self.expected = None
        self.payload_start = 0
        self.scanned_chars = 0
        self.scanned_units = 0
        self.prefix_checked = False

    def feed(self, content: str) -> list:
        if content:
            self.buffer += content
        self._strip_prefix()
        frames = []
        while (self.expected is not None or self._read_length()) and self.expected is not None:
            self._scan()
            if self.scanned_units < self.expected:
                break
            end = self.payload_start + self.scanned_chars
            chunk = self.buffer[self.payload_start:end]
            self.buffer = self.buffer[end:]
            self.expected = None
            self.payload_start = 0
            self.scanned_chars = 0
            self.scanned_units = 0
            if not chunk.strip():
                continue
            try:
                parsed = json.loads(chunk)
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, list):
                frames.extend(parsed)
            else:
                frames.append(parsed)
        return frames

    def _strip_prefix(self):
        if self.prefix_checked:
            return
        prefix = ")]}'"
        if len(self.buffer) < len(prefix) and prefix.startswith(self.buffer):
            return
        if self.buffer.startswith(prefix):
            self.buffer = self.buffer[len(prefix):].lstrip()
        self.prefix_checked = True

    def _read_length(self) -> bool:
        self.buffer = self.buffer.lstrip()
        match = _LENGTH_RE.match(self.buffer)
        if not match:
            return False
        digits = match.group(1)
        self.expected = int(digits)
        self.payload_start = len(digits)
        self.scanned_chars = 0
        self.scanned_units = 0
        return True

    def _scan(self):
        index = self.payload_start + self.scanned_chars
        while self.scanned_units < self.expected and index < len(self.buffer):
            units = 2 if ord(self.buffer[index]) > 0xFFFF else 1
            if self.scanned_units + units > self.expected:
                break
            self.scanned_units += units
            self.scanned_chars += 1
            index += 1


def parse_init_page(html: str) -> dict:
    def grab(pattern):
        match = pattern.search(html)
        return match.group(1) if match else None

    token = grab(_ACCESS_TOKEN_RE)
    if not token:
        raise ProtocolError(
            "Gemini did not return a session token. The saved cookies may have expired."
        )
    return {
        "access_token": token,
        "build_label": grab(_BUILD_LABEL_RE),
        "session_id": grab(_SESSION_ID_RE),
        "language": grab(_LANGUAGE_RE) or "en",
    }


def encode_frame(payload) -> str:
    """Test helper. Builds one length-prefixed frame the parser accepts."""
    body = json.dumps(payload, separators=(",", ":"))
    framed = "\n" + body
    return f"{_utf16_units(framed)}\n{body}"


def build_generate_body(prompt: str, access_token: str, language: str, metadata, model: ModelChoice | None, session_id: str) -> tuple[dict, dict]:
    """Return (form fields, extra headers) for StreamGenerate."""
    chat_meta = _DEFAULT_METADATA.copy()
    if isinstance(metadata, list):
        for index, value in enumerate(metadata[:10]):
            if value is not None:
                chat_meta[index] = value

    message = [prompt, 0, None, None, None, None, 0]
    inner = [None] * 81
    inner[0] = message
    inner[1] = [language]
    inner[2] = chat_meta
    inner[6] = [1]
    inner[7] = 1
    inner[10] = 1
    inner[11] = 0
    inner[17] = [[0]]
    inner[18] = 0
    inner[27] = 1
    inner[30] = [4]
    inner[41] = [1]
    inner[53] = 0
    inner[59] = str(uuid.uuid4()).upper()
    inner[61] = []
    inner[68] = 1
    inner[79] = 1
    inner[80] = 1

    headers = {
        **_PAGE_HEADERS,
        "X-Same-Domain": "1",
        "x-goog-ext-525005358-jspb": f'["{inner[59]}",1]',
    }
    if model is not None:
        header = json.loads(model.header_value())
        if isinstance(header[-1], int):
            inner[79] = header[-1]
        header.extend([1, session_id])
        headers[MODEL_HEADER] = json.dumps(header, separators=(",", ":"))
        headers["x-goog-ext-73010989-jspb"] = "[0]"
        headers["x-goog-ext-73010990-jspb"] = "[0,0,0]"

    form = {
        "at": access_token,
        "f.req": json.dumps([None, json.dumps(inner, separators=(",", ":"))], separators=(",", ":")),
    }
    return form, headers


def parse_generate_frames(frames: list) -> tuple[str, list, list]:
    """Pull the latest reply text, chat metadata, and image descriptors from stream frames."""
    text = ""
    metadata: list = []
    media: list = []
    for part in frames:
        if not isinstance(part, list):
            continue
        error_code = _nested(part, [5, 2, 0, 1, 0])
        if error_code:
            raise ProtocolError(f"Gemini rejected the request (code {error_code}).")
        inner_raw = _nested(part, [2])
        if not isinstance(inner_raw, str):
            continue
        try:
            inner = json.loads(inner_raw)
        except json.JSONDecodeError:
            continue
        meta = _nested(inner, [1])
        if isinstance(meta, list) and meta:
            metadata = meta
        cid = _nested(meta, [0], "") if isinstance(meta, list) else ""
        rid = _nested(meta, [1], "") if isinstance(meta, list) else ""
        for candidate in _nested(inner, [4], []) or []:
            if not isinstance(candidate, list):
                continue
            rcid = _nested(candidate, [0], "")
            candidate_text = _nested(candidate, [1, 0], "") or ""
            if isinstance(candidate_text, str) and _CARD_RE.match(candidate_text):
                candidate_text = _nested(candidate, [22, 0]) or candidate_text
            if isinstance(candidate_text, str) and candidate_text:
                text = _ARTIFACTS_RE.sub("", candidate_text)
            rich = _nested(candidate, [12])
            for image in _field(rich, 1, []) or []:
                url = _nested(image, [0, 0, 0])
                if isinstance(url, str) and url:
                    media.append({
                        "type": "web_image",
                        "meta": {
                            "url": url,
                            "title": _nested(image, [0, 4], "") or "",
                            "alt": _nested(image, [0, 4], "") or "",
                        },
                    })
            generated = _nested(_field(rich, 7), [0], []) or []
            for image in generated:
                url = _nested(image, [0, 3, 3])
                if isinstance(url, str) and url:
                    media.append({
                        "type": "generated_image",
                        "meta": {
                            "url": url,
                            "title": "",
                            "alt": _nested(image, [0, 3, 2], "") or "",
                            "cid": cid,
                            "rid": rid,
                            "rcid": rcid,
                            "image_id": _nested(image, [1, 0], "") or "",
                        },
                    })
    return text, metadata, media


def _capacity(tier_flags, capability_flags) -> tuple[int, int]:
    tier_flags = tier_flags if isinstance(tier_flags, list) else []
    capability_flags = capability_flags if isinstance(capability_flags, list) else []
    if 21 in tier_flags:
        return 1, 13
    if 22 in tier_flags:
        return 2, 13
    if 115 in capability_flags:
        return 4, 12
    if 16 in tier_flags or 106 in capability_flags:
        return 3, 12
    if 8 in tier_flags or 19 in capability_flags:
        return 2, 12
    return 1, 12


def _models_from_status(body) -> list[ModelChoice]:
    capacity, capacity_field = _capacity(_nested(body, [16], []), _nested(body, [17], []))
    models = []
    for item in _nested(body, [15], []) or []:
        if not isinstance(item, list):
            continue
        model_id = _nested(item, [0], "")
        if not isinstance(model_id, str) or not model_id:
            continue
        category = str(_nested(item, [1], "") or _nested(item, [10], "") or "")
        display = str(_nested(item, [11], "") or _nested(item, [19], "") or category)
        number = _nested(item, [17])
        if not isinstance(number, int):
            number = _nested(item, [9]) if isinstance(_nested(item, [9]), int) else 1
        slug = category.strip().lower().replace(" ", "-")
        name = f"gemini-{slug}" if slug else f"gemini-{model_id}"
        aliases = {name, model_id.lower(), category.strip().lower(), display.strip().lower()}
        aliases.add(display.strip().lower().replace(" ", "-"))
        models.append(ModelChoice(model_id, name, capacity, capacity_field, number, aliases))
    return models


def _reply_complete(frames: list) -> bool:
    """Gemini marks a finished candidate with 2 in field 8."""
    for part in frames:
        if not isinstance(part, list):
            continue
        raw = part[2] if len(part) > 2 and isinstance(part[2], str) else None
        if not raw:
            continue
        try:
            inner = json.loads(raw)
        except json.JSONDecodeError:
            continue
        for candidate in _nested(inner, [4], []) or []:
            if _nested(candidate, [8, 0]) == 2:
                return True
    return False


def _unusable_reply(text: str) -> bool:
    """A model-tagged request sometimes comes back as a generic failure sentence."""
    if not text or not text.strip():
        return True
    folded = text.strip().lower()
    openings = (
        "sorry, something went wrong",
        "i seem to be encountering an error",
        "i encountered an error",
        "i'm having a hard time fulfilling your request",
    )
    return folded.startswith(openings)


def _resolve_model(models: list[ModelChoice], name: str | None) -> ModelChoice | None:
    if not name:
        return None
    target = name.lower().strip()
    for model in models:
        if target == model.model_name or target in model.aliases or target == model.model_id.lower():
            return model
    # "gemini-flash" should still hit a discovered "gemini-3-flash" style name.
    tail = target.removeprefix("gemini-")
    for model in models:
        if tail and tail in model.model_name:
            return model
    if models:
        known = ", ".join(model.model_name for model in models)
        raise ProtocolError(f"Unknown model '{name}'. This account offers: {known}")
    return None


class GeminiSession:
    def __init__(self, sid: str, sidts: str):
        self.sid = sid
        self.sidts = sidts
        self.client = None
        self.access_token = ""
        self.build_label = None
        self.session_id = None
        self.language = "en"
        self.models: list[ModelChoice] = []
        self._reqid = random.randint(10000, 99999)
        self._sessionid = str(uuid.uuid4()).upper()
        self.quota = "Quota: Unknown"
        self.abuse = "Abuse Status: Unknown"

    async def open(self):
        from curl_cffi.requests import Session

        self.client = Session(impersonate=BROWSER, allow_redirects=True)
        self._set_cookies()
        response = self.client.get(INIT_URL, headers=_PAGE_HEADERS, timeout=20)
        if response.status_code != 200:
            raise ProtocolError(f"Gemini sign-in page returned HTTP {response.status_code}.")
        parsed = parse_init_page(response.text)
        self.access_token = parsed["access_token"]
        self.build_label = parsed["build_label"]
        self.session_id = parsed["session_id"]
        self.language = parsed["language"]
        # Model and quota RPCs are optional. A slow one must not block the reply.
        for loader in (self._load_models, self._load_quota):
            try:
                await asyncio.wait_for(loader(), timeout=15)
            except Exception:
                pass

    def _set_cookies(self):
        self.client.cookies.set("__Secure-1PSID", self.sid, domain=".google.com", path="/", secure=True)
        self.client.cookies.set("__Secure-1PSIDTS", self.sidts, domain=".google.com", path="/", secure=True)

    async def close(self):
        if self.client is not None:
            self.client.close()
            self.client = None

    async def generate(self, prompt: str, model_name: str | None = None, metadata=None) -> Reply:
        if self.client is None:
            raise ProtocolError("Session is not open.")
        if not prompt.strip():
            raise ProtocolError("Prompt cannot be empty.")
        model = _resolve_model(self.models, model_name)
        text, meta, media = self._read_generation(prompt, model, metadata)
        if model is not None and _unusable_reply(text):
            text, meta, media = self._read_generation(prompt, None, metadata)
        if not text:
            raise ProtocolError("Gemini returned no reply text.")
        return Reply(text, meta or metadata or [], media, self.quota, self.abuse)

    def _read_generation(self, prompt, model, metadata):
        """Read one StreamGenerate response. Return as soon as reply text arrives.

        A model header can make Gemini send the chat id and then stop. The caller
        retries without that header. Waiting for the socket to close hits the
        60 second curl timeout with only the first packet in hand.
        """
        form, headers = build_generate_body(
            prompt, self.access_token, self.language, metadata, model, self._sessionid
        )
        params = {"hl": self.language, "_reqid": self._reqid, "rt": "c"}
        self._reqid += 100000
        if self.build_label:
            params["bl"] = self.build_label
        if self.session_id:
            params["f.sid"] = self.session_id

        parser = FrameParser()
        frames = []

        def parsed():
            text, meta, media = parse_generate_frames(frames)
            return text, meta, media

        try:
            with self.client.stream(
                "POST", GENERATE_URL, params=params, headers=headers, data=form, timeout=12
            ) as response:
                if response.status_code != 200:
                    raise ProtocolError(
                        f"Generation failed with HTTP {response.status_code}: {response.text[:180]}"
                    )
                for chunk in response.iter_content():
                    piece = chunk.decode("utf-8", "replace") if isinstance(chunk, bytes) else chunk
                    frames.extend(parser.feed(piece))
                    text, meta, media = parsed()
                    if text and _reply_complete(frames):
                        return text, meta, media
        except ProtocolError:
            raise
        except Exception:
            pass
        frames.extend(parser.feed(""))
        return parsed()

    async def save_url(self, url: str, dest_dir: Path) -> str:
        from curl_cffi.requests import Session

        temporary = self.client is None
        if temporary:
            self.client = Session(impersonate=BROWSER, allow_redirects=True)
            self._set_cookies()
        try:
            response = await self._download(url, dest_dir)
        finally:
            if temporary:
                await self.close()
        return response

    async def _download(self, url: str, dest_dir: Path) -> str:
        response = self.client.get(
            url, headers={"Referer": "https://gemini.google.com/"}, timeout=60
        )
        if response.status_code != 200:
            raise ProtocolError(f"Download failed with HTTP {response.status_code}.")
        dest_dir.mkdir(parents=True, exist_ok=True)
        suffix = ".png"
        content_type = response.headers.get("content-type", "")
        if "jpeg" in content_type:
            suffix = ".jpg"
        elif "webp" in content_type:
            suffix = ".webp"
        elif "gif" in content_type:
            suffix = ".gif"
        elif "mp4" in content_type:
            suffix = ".mp4"
        path = dest_dir / f"{uuid.uuid4().hex}{suffix}"
        path.write_bytes(response.content)
        return str(path.resolve())

    async def _batch(self, rpc_id: str, payload: str) -> list:
        assert self.client is not None
        params = {
            "rpcids": rpc_id,
            "hl": self.language,
            "_reqid": self._reqid,
            "rt": "c",
            "source-path": "/app",
        }
        self._reqid += 100000
        if self.build_label:
            params["bl"] = self.build_label
        if self.session_id:
            params["f.sid"] = self.session_id
        header = json.loads(
            "[1,null,null,null,null,null,null,null,[4,5,6,8],null,null,null,null,null,null,null]"
        )
        header.append(self._sessionid)
        headers = {
            **_PAGE_HEADERS,
            "X-Same-Domain": "1",
            MODEL_HEADER: json.dumps(header, separators=(",", ":")),
            "x-goog-ext-73010989-jspb": "[0]",
        }
        form = {
            "at": self.access_token,
            "f.req": json.dumps([[[rpc_id, payload, None, "generic"]]], separators=(",", ":")),
        }
        response = self.client.post(BATCH_URL, params=params, headers=headers, data=form, timeout=12)
        if response.status_code != 200:
            raise ProtocolError(f"RPC {rpc_id} failed with HTTP {response.status_code}.")
        parser = FrameParser()
        frames = parser.feed(response.text)
        frames.extend(parser.feed(""))
        bodies = []
        for part in frames:
            if _nested(part, [1]) != rpc_id:
                continue
            raw = _nested(part, [2])
            if not isinstance(raw, str):
                continue
            try:
                bodies.append(json.loads(raw))
            except json.JSONDecodeError:
                continue
        return bodies

    async def _load_models(self):
        for body in await self._batch(USER_STATUS_RPC, "[]"):
            self.models = _models_from_status(body)
            if self.models:
                return

    async def _load_quota(self):
        lines = []
        for body in await self._batch(QUOTA_RPC, FLASH_QUOTA_PAYLOAD):
            items = _nested(body, [0], [])
            if not isinstance(items, list):
                continue
            for item in items:
                action = _nested(item, [0, 1])
                label = {4: "Gemini Pro", 11: "Gemini Flash", 15: "Gemini Flash Thinking"}.get(
                    action, "Gemini"
                )
                total = _nested(item, [4])
                remaining = _nested(item, [5])
                if total == 0 and remaining == 0:
                    lines.append(f"{label}: Unlimited")
                elif total is not None and remaining is not None:
                    lines.append(f"{label}: {remaining}/{total} credits remaining")
        if lines:
            self.quota = " | ".join(lines)
