# Deezer source

Built on [deezsrc](https://github.com/anon7627/deezsrc) by
[anon7627](https://github.com/anon7627), also the author of
[Ricercar](https://github.com/anon7627/ricercar-qconnect). The original MIT
licence and copyright are preserved in LICENSE and NOTICE.

Native discovery, search, saved library and playback for Deezer. Packages are
provided for Linux x86_64 and macOS Apple Silicon. Each package contains one
executable; no Python installation or companion process is required.

The source has its own sidebar entry, Home, saved albums, tracks, artists and
playlists. With a connected account, Home reads Deezer's personalised music
feed, including supported artist, playlist, album and song sections. Shelves
with a full module offer See all, starting with their first items and continuing
through the complete listing. Without an account, Home shows public releases
and charts. Album shelves expose the player's sorting and grouping controls.
Library ordering uses the metadata supplied by the service; unavailable dates
are not fabricated. Audio requires your own account and the access granted to
it by Deezer. FLAC and MP3 are maximum requested qualities; availability and
subscription checks remain in the upstream implementation.

Connected artist pages show top tracks, the latest release, albums, EPs,
singles, live albums, compilations and featured releases using Deezer's own
categories. Discography sections load 30 releases at a time. Related artists
and playlists provide six-item previews. Album reads use the account gateway
for their header and songs together, with bounded continuation for larger
albums. Public browsing remains available when signed out.

Artist sections use the native page roles for Popular Tracks, Latest release
and Albums, in that order. EPs and singles retain separate provider categories.
Popular Tracks includes up to 200 rows already present in the artist response,
with a five-track folded preview and a continuation for incomplete responses.
Discography continuations remain explicit; the plugin does not fetch later
pages in the background. Expansion, scroll restoration and the highlighted
release's navigation controls belong to the player.

Create playlists in your account and add up to 100 source tracks per request.
Owned playlists also support title, description and visibility changes,
deletion and track search in the native editor. Ownership is checked before
each write. Writes are never automatically retried; if a change cannot be
confirmed, inspect the playlist before trying again. Copying, per-entry removal
and reordering are not exposed. Flow mood controls, podcasts, concerts and
other web-only destinations are not included in the native music feed.

## Connect your account

Open the plugin's settings and follow the three-step setup:

1. Sign in on Deezer's website, or choose **Continue** if you are already
   signed in. Opening the website alone does not connect the plugin.
2. Open your browser's developer tools. Firefox: Storage → Cookies. Chrome
   or Edge: Application → Storage → Cookies. Safari: enable developer features
   under Settings → Advanced, then Develop → Show Web Inspector → Storage.
   Select the Deezer domain and copy the **Value** of your own `arl` cookie.
   Paste it into the private session field and choose **Connect**.
3. The plugin checks the session and shows the connected account. Browse Home,
   search or open your saved library, then play a track. The quality selector
   sets the maximum requested format. Failed or expired sessions stay on the
   setup screen with recovery instructions.

ARL grants access to your account. Enter it only in the local password field;
never include it in issues, logs or chat. The plugin does not inspect or share
your browser's stored cookies and cannot automatically import its session.

## Storage and privacy

The validated session is stored only in this plugin's private data directory,
in `account.json`, with permissions 0600 on the supported systems. It is not
encrypted at rest and does not use an OS keychain. Settings never return the
session value. Sign out removes it and revokes the local relay; audio already
buffered by the player may drain briefly. Your browser remains signed in.

Public catalogue requests go to Deezer. Account and playback requests use
your own session; artwork may be fetched from the service's CDN. This adapter
does not enable playback reporting or collect telemetry. It includes no audio
export, download command or preview substituted for full playback. It never
reads another plugin's credentials.

Artist category queries exchange the existing session with `auth.deezer.com`
and query `pipe.deezer.com`. JWT and refresh tokens stay in memory, are bounded
to 16 KiB each, and are discarded on sign-out or account replacement.

## Build and package

Use stable Rust with the checked-in lockfile. Run `cargo test --locked --
--test-threads=1` and `cargo build --locked --release`. Tests use generated
fixtures and a local fake service; no account or speakers are needed. Build
on each target system, then run:

```
python3 tools/package.py --binary target/release/plugin --platform linux-x86_64
python3 tools/package.py --binary target/release/plugin --platform macos-aarch64
```

Use the command for the binary you actually built. The packager checks the
executable format and architecture. A Linux binary cannot be repackaged as a
Mac build. The macOS executable may carry an ad hoc signature; it is not
notarised. Intel Mac, Windows and Android packages are not provided.

Native protocol version 1 is used over standard input/output. The first call
is `initialize` with private `data_dir`, `cache_dir`, locale and network state.
The process follows SDK revision `76a4ad5`: both `data_dir` and `cache_dir`
must be absolute paths in `initialize`. Missing or relative folders reject
initialization before storage opens. Account state stays in `data_dir`;
the adapter does not need a disk catalogue cache or an export directory.

Only implemented native capabilities are declared. The process has two Tokio
workers, four request workers, 16 pending requests and a 1 MiB line limit.
Search is bounded to 50 entries per type, collections to 10,000 tracks, and
library reads aggregate the upstream's 200-entry pages into 500-entry pages.
The upstream's four account caches retain at most 10,000 entries each for a
minute. The relay retains at most 64 expiring stream tickets. Process exit
releases workers and the relay; sign-out clears account caches and tickets.
Personalised Home returns at most 32 shelves with 24 preview cards each.
Module listings read at most 1,000 supported entities and return 200 per page;
larger or incomplete modules report an error. Discovery adds no cache, worker
or polling loop. Playlist writes share one mutex within the existing bounded
request queue.
Artist pages keep at most two category requests in flight per page, within
the four existing request workers. Each GraphQL response is limited to 2 MiB,
authentication responses to 64 KiB, and the single account token cache to
five minutes or the token's shorter lifetime. No additional worker, timer or
disk catalogue cache is used.
Rate-limited catalogue reads share one retry deadline without a polling task.
An explicit refresh may retry immediately; account settings and stream
resolution retain their existing request paths.

## Verification limits

Automated checks cover protocol, account forms, local fixture authentication,
library pagination and upstream stream-format/range behaviour. Native package
and player integration checks are reported with each release. A fake account
or generated audio does not establish live service playback or physical
audibility. Real account and output checks must be identified separately.

Built from [anon7627/deezsrc](https://github.com/anon7627/deezsrc), MIT.
See LICENSE and NOTICE for the upstream attribution. Deezer is a trademark of
its respective owner. This independent project is not endorsed by Deezer.
Use the plugin only with an account and content you are entitled to access.
The software is provided without warranty; see LICENSE.
