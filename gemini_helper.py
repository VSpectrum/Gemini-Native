"""Single entry point for the bundled app.

One PyInstaller directory is about half the size of two onefile binaries, and
it does not unpack itself into a temp folder on every message.
"""

import asyncio
import sys

# Module scope so PyInstaller follows them into the bundle. browser_session
# comes in through gemini_auto; gemini_webapi comes in through both helpers.
import browser_session  # noqa: F401
import gemini_auto
import gemini_download


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
