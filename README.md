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

Configuration is by environment variables: the Easynews login, an API key of your choosing (used for
both endpoints), the optional address and key of a real SABnzbd to front, and the public URL the apps
will use to reach this service.

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
- The provided container image (a single static `x86_64-unknown-linux-musl` binary; no runtime
  dependencies), or a Rust 1.85+ toolchain to build it yourself.
- A folder the apps and this service both see under the same path (the "completed" folder).

## Build and run

```
docker build -t easynews-nzb .
```

produces a container around a single static binary (`app/`, a Rust crate with no runtime
dependencies once built). Copy `docker-compose.example.yml` to `docker-compose.yml`, fill in the
environment values (see `docs/DESIGN.md` section 2), and run:

```
docker compose up -d
```

To build just the binary (for `x86_64-unknown-linux-musl`) without Docker:

```
cd app && cargo build --release --target x86_64-unknown-linux-musl
```

## Legal

This is an independent project and is not affiliated with Easynews. It uses the same members-only
search and download endpoints that Easynews' own web interface and the third-party Easynews clients
use, with your own account. Respect the provider's terms and keep request rates modest.
