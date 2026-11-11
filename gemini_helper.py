"""Single entry point for the bundled app.

One PyInstaller directory is about half the size of two onefile binaries, and
it does not unpack itself into a temp folder on every message.
"""

import asyncio
import sys

# Module scope so PyInstaller follows them into the bundle.
import browser_session  # noqa: F401
import gemini_auto
import gemini_download
import gemini_protocol  # noqa: F401


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else "serve"
    if command == "download":
        sys.argv = [sys.argv[0], *sys.argv[2:]]
        asyncio.run(gemini_download.main())
        return
    if command == "prompt":
        sys.argv = [sys.argv[0], *sys.argv[2:]]
    elif command == "serve":
        sys.argv = [sys.argv[0], "--serve"]
    asyncio.run(gemini_auto.main())


if __name__ == "__main__":
    main()
