# easynews-nzb

A Newznab indexer and a SABnzbd-compatible download client in one small service, backed by an
[Easynews](https://www.easynews.com/) account.

## What it is for

Easynews is a Usenet provider with a difference: its servers have already fetched every article,
joined the parts, run the par2 repair and unpacked the archives. Its search returns finished files,
each one a plain HTTPS download. No NZB, no NNTP connections, no repair, no extraction.

The usual home media stack (Prowlarr, Radarr, Sonarr, Lidarr and their relatives) cannot use that
directly. It expects two separate things:

- an **indexer** that answers Newznab searches with NZB files, and
- a **download client** such as SABnzbd that turns an NZB into files on disk.

easynews-nzb plays both roles at once. To Prowlarr it looks like a Newznab indexer whose searches
are Easynews searches. To Radarr, Sonarr and friends it looks like a SABnzbd instance. When an app
"grabs" a release, the NZB it passes around is a small ticket that leads back to this service, which
downloads the already-unpacked file from Easynews over HTTPS into the completed folder the apps
expect, and reports it in the SABnzbd-style history so the app imports it as usual.

So one Easynews subscription replaces both an indexer subscription and the downloader.

## How it fits next to a real SABnzbd

If you also have ordinary NZB indexers (NZBgeek and the like) and a real SABnzbd, easynews-nzb can
sit **in front** of it: every app points at easynews-nzb as its only SABnzbd. NZBs that carry an
Easynews ticket are handled here; every other NZB is forwarded to the real SABnzbd unchanged, and
the queue and history calls are merged so the apps see a single client. This matters for apps that
accept only one download client.

Without a real SABnzbd, easynews-nzb runs alone and simply answers "nothing else in the queue".

## Endpoints

| Path | Role |
|---|---|
| `/api` | Newznab: `t=caps`, `t=search`, `t=tvsearch`, `t=movie`, `t=music` |
| `/api/nzb/{ticket}` | The NZB for a search result (a ticket, see the design document) |
| `/sab/api` | SABnzbd-compatible API: `version`, `get_config`, `get_cats`, `addfile`, `addurl`, `queue`, `history`, deletes, pause/resume |
| `/health` | Status JSON |

See "Configuration" below for how to set it up.

## Status

Implemented in Rust and verified live end-to-end: real Newznab searches and ticket NZBs, a real
SABnzbd instance fronted correctly in both directions (Easynews tickets handled locally, everything
else forwarded unchanged), and real film, TV episode, and music grabs through Prowlarr/Radarr/Sonarr
each downloaded via Easynews and imported successfully. See `docs/DESIGN.md` for the full
specification: the verified Easynews API (search parameters, result fields, download URL forms), the
exact Newznab and SABnzbd shapes the apps rely on, the ticket NZB, the download job model, and the
test plan.

## Requirements

- An Easynews account (the same username and password used for NNTP access).
- Docker, or a Rust 1.85+ toolchain if you'd rather build it yourself (see "Compile from source").
- A folder the apps and this service both see under the same path (the "completed" folder).

## Configuration

Everything is configured by environment variables — there's no config file to write by hand.

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `EASYNEWS_USERNAME` | yes | — | Your Easynews account username (same login as NNTP) |
| `EASYNEWS_PASSWORD` | yes | — | Your Easynews account password |
| `API_KEY` | yes | — | A key you choose. Both the Newznab and SABnzbd-compatible endpoints require it as `apikey=` |
| `PUBLIC_URL` | yes | — | The base URL your other apps use to reach this service, e.g. `http://192.0.2.10:8090` — used to build the NZB download links Prowlarr hands out |
| `SAB_URL` | no | unset | A real SABnzbd to front, e.g. `http://sabnzbd:8080`. Leave this and `SAB_API_KEY` unset to run standalone (see "How it fits next to a real SABnzbd" above) |
| `SAB_API_KEY` | no | unset | API key for the real SABnzbd above; must be set together with `SAB_URL` |
| `INCOMPLETE_DIR` | no | `/downloads/incomplete` | Where in-progress downloads are written |
| `COMPLETE_DIR` | no | `/downloads/complete` | Where finished downloads land, under `<COMPLETE_DIR>/<category>/<release>/` — this is the path your apps must see as the completed-downloads folder |
| `STATE_FILE` | no | `/config/state.json` | Where tickets and job state are persisted (plain JSON, rewritten atomically) |
| `PORT` | no | `8090` | Port to listen on |
| `UMASK` | no | `002` (octal) | Applied once at startup, before any file is created, so downloaded files and folders come out with consistent permissions |

`INCOMPLETE_DIR` and `COMPLETE_DIR` must resolve to the same actual folders for both this service
and whatever imports from them (Radarr, Sonarr, ...) — mount the same host directory into every
container at the same path.

## Run with Docker (recommended)

```bash
git clone https://github.com/leancode/easynews-nzb.git
cd easynews-nzb
cp docker-compose.example.yml docker-compose.yml
```

Edit `docker-compose.yml`: fill in `EASYNEWS_USERNAME`, `EASYNEWS_PASSWORD`, `API_KEY` and
`PUBLIC_URL` from the table above, and set `user: "UID:GID"` to whoever owns your downloads folder
on the host. Then:

```bash
docker compose up -d --build
```

and check it's up:

```bash
curl http://localhost:8090/health
```

## Compile from source

Needs a Rust 1.85+ toolchain ([rustup.rs](https://rustup.rs)).

```bash
cd app
cargo build --release
```

builds `target/release/easynews-nzb` for your host platform. Run it directly with the environment
variables from the table above set, for example:

```bash
EASYNEWS_USERNAME=you EASYNEWS_PASSWORD=pass API_KEY=changeme PUBLIC_URL=http://localhost:8090 \
  ./target/release/easynews-nzb
```

To build the same static, dependency-free `x86_64-unknown-linux-musl` binary the Docker image
ships (needs a musl C toolchain — `musl-tools` on Debian/Ubuntu — or just use the `Dockerfile`,
which handles that for you in a container):

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Point your apps at it

- **Prowlarr**: Indexers → Add Indexer → "Generic Newznab". URL is your `PUBLIC_URL`, API path
  `/api`, API key your `API_KEY`. Test, then Save — Prowlarr syncs it to any connected Radarr/Sonarr
  automatically.
- **Radarr / Sonarr**: Settings → Download Clients → Add → SABnzbd. Host and port from
  `PUBLIC_URL`, URL base `/sab`, API key your `API_KEY`.
- **Anything else with a SABnzbd-compatible client field** (LazyLibrarian, Lidarr, Readarr, ...):
  same idea — host, port, URL base `/sab`, your `API_KEY`.

## Legal

This is an independent project and is not affiliated with Easynews. It uses the same members-only
search and download endpoints that Easynews' own web interface and the third-party Easynews clients
use, with your own account. Respect the provider's terms and keep request rates modest.
