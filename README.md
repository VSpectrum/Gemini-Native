# Rant of a newly made NEET

As an almost lifelong Pythonist and the required enterprise Java (-.-) I don't speak a lick of Rust but I've tried dabbing in it over the recent years. Those stints never stuck since I valued the productivity I got out of Python more I suppose... Yeah yeah, learning curve skill issues on my part.

DHH's spiel refocused Rust back on the radar but more as a vibe-coding target that I can whip some stuff up for.

Anyways here's the rationale behind this quick project:
I gotta bunch of Macbook 2012s (Retina and non-Retina) idk I had a Mac phase where I decided to collect some older machines in 2021. Despite my reseating / reapplying thermal paste on the CPU (and GPU for Retina model) these guys be screaming at me when I'm doing dev + browsing on them.
I've tried to religiously monitor my Activity Monitor to make sure nothing's up. I'm on SSD equivalents and the RAM's okayish but all in all, all these Electron based apps and variant of Chromium (as well as being on unsupported OSes via OpenCore), they're just not having it.

Here's what I'm testing this on:

<img width="254" height="188" alt="Screenshot 2026-10-04 at 9 17 10 AM" src="https://github.com/user-attachments/assets/b7f935c0-8480-4b63-bd85-02adb28caa5f" />


So here we are, exploring a rust based browser for interacting with my _fav_ app. _Gemini_

I needed the 5TB space guys. So now I'm along for the ride in this Google AI world.

I'm also this close to dropping a 20 on Claude and supporting the Anthropic overlords.

Don't even ask about Cursor...

Anyways so far this has been a successful endeavor for my little Macbooks. They are happy, they can view remotely generated LLM generated text without having to blow a fuse on that Electron/Chromium bus.
Heck even WebKit fired up the fans so I can't just drag Chromium / V8 alone for this...

Now I can use _Gemini_ not in a browser and it's not going to make my old revitalized laptop take off ✈️ (this emoji isn't LLM generated!)

# Setup & Installation

If you are downloading a pre-built release for your operating system, the Python scripts and dependencies are bundled natively, but you still need to ensure Playwright's Chromium browser is installed on your system so the app can securely grab your Gemini session cookies.

## 1. Run the App & Get Cookies
1. Open the downloaded `Gemini Native Client` app (or `.exe` on Windows).
2. Type any simple message (e.g., "Hello") and press Enter.
3. The app will detect you don't have cookies yet and will automatically download Chromium (if not already installed) and pop open a visible browser window.
4. **Log in to your Google Account** in that browser window.
5. Once logged in, the browser will automatically close, save your secure session cookies locally, and the Rust UI will take over!

## For Developers (Building from source)
If you are developing locally:
```bash
pip install -r requirements.txt # (or install playwright and gemini_webapi manually)
playwright install chromium
cargo run
```

## Cache locations in your home directory

CACHE_DIR = Path.home() / ".gemini_local"

COOKIE_FILE = CACHE_DIR / "cookies.json"

PROFILE_DIR = CACHE_DIR / "chrome_profile"

UI State File: ~/Library/Application Support/Gemini Native Client/data/app.ron


# Final Results

<img width="1920" height="1142" alt="Screenshot 2026-10-04 at 10 28 06 AM" src="https://github.com/user-attachments/assets/64a79a64-7c62-4f17-9b38-d6740fb0e86d" />

<img width="824" height="19" alt="Screenshot 2026-10-04 at 10 29 39 AM" src="https://github.com/user-attachments/assets/1716a469-3906-4f7f-acce-4cb5b0e5c6f9" />
<img width="767" height="21" alt="Screenshot 2026-10-04 at 10 29 01 AM" src="https://github.com/user-attachments/assets/7fb1906d-1173-48f2-aca2-a99908ec3414" />

I'm not thrilled that this still uses 100MB of RAM but if I made this a TUI instead of a GUI we wouldnt have markdown and other nice things. Allegedly if we went down the TUI route (raratui) we could've been running this on 5-20MB of RAM. Also if I am able to gut out the playwright requirements I can drop the Python and have this not be > 100MB in filesize >.<

But for now we'll stick with Playwright...

