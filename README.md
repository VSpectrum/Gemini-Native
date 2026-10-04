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

# Setup

```
cargo run
```

login to ur gemini via playwright so it can grab ur cookies and leave.

It'll then feed ur cookies to the daemons.. In this case the rust app which acts as the GUI and session manager for resource optimal text viewer.

Hey I wrote all this without an LLM! This is basically rehab. woohoo

## Cache locations in your home directory
CACHE_DIR = Path.home() / ".gemini_local"
COOKIE_FILE = CACHE_DIR / "cookies.json"
PROFILE_DIR = CACHE_DIR / "chrome_profile"

UI State File: ~/Library/Application Support/Gemini Native Client/data/app.ron
