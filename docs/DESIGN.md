# Design

One Rust process (axum + tokio + reqwest, no database), compiled as a single static
`x86_64-unknown-linux-musl` binary, that is a Newznab indexer and a SABnzbd-compatible download
client, backed by Easynews' members-only search and download endpoints. Everything in section 1 was
verified against a live account on 2026-09-30.

## 1. Easynews API

Authentication: HTTP Basic with the Easynews username and password (the same login as for NNTP).
Always send a `User-Agent` such as `easynews-nzb/1.0`.

### 1.1 Search

`GET https://members.easynews.com/2.0/search/solr-search/advanced`

| param | meaning |
|---|---|
| `gps` | search words (matched against subject and file name) |
| `sb` | `1` (sort; keep `1`) |
| `pby` | results per page (use 100) |
| `pno` | page number, 1-based |
| `st` | `adv` |
| `sS` | `0` = JSON (`5` = RSS, not useful here) |
| `fty[]` | type filter, repeatable: `VIDEO`, `AUDIO`, `IMAGE`, `OTHER` |
| `fex` | extension filter such as `mkv` (optional; test before relying on it) |

Top-level response keys: `sid` (session download token, valid for hours), `results` (total),
`perPage`, `numPages`, `page`, `dlFarm` (`auto`), `dlPort` (`443`), `downURL`
(`https://members.easynews.com/dl`), `groups` (newsgroup histogram), `data` (the files).

Each `data` item carries Easynews' numeric column ids and named duplicates; use the named ones:

| key | content |
|---|---|
| `0` / `hash` | file hash with the 4-hex file id appended (`id` holds the id alone) |
| `10` / `fn` | file name without extension |
| `11` / `extension` | extension with the dot (`.flac`, `.mkv`) |
| `4`, `rawSize` / `size` | size as text, and in bytes |
| `5`, `ts` / `timestamp` | post date as text, and epoch seconds |
| `6` / `subject` | subject (often obfuscated) |
| `7` / `poster` | poster |
| `9` / `groups`, `group_list` | newsgroup(s) |
| `type` | `VIDEO`, `AUDIO`, `IMAGE`, `OTHER` |
| `vcodec`, `acodec`, `fullres`, `runtime` (seconds), `bps`, `hz`, `alangs`, `slangs` | media info |
| `setid` / `colid` | the post set the file belongs to; files of one album or season share it |
| `passwd` / `password`, `virus` | skip the file when either is true |
| `sig` | per-file download signature |
| `expires` | `&#8734;` means never |

### 1.2 Download

Both forms answer `200` with the finished file and its real `Content-Length`:

- Session form: `{downURL}/{dlFarm}/{dlPort}/{hash}{extension}/{urlencoded fn}{extension}?sid={sid}`
- Per-file form: `https://members.easynews.com/dl/auto/443/{hash}{extension}/{urlencoded fn}{extension}?sig={sig}`

Same Basic auth header. Stream with `Range` support so a broken transfer resumes. On `401`/`403`,
repeat the original search once to obtain a fresh `sig` and retry.

Politeness: at most one search per second, at most two parallel downloads. Never log `sig` or `sid`.

## 2. Service layout

Port 8090 by default.

| Path | Role |
|---|---|
| `GET /api` | Newznab (`t=caps`, `search`, `tvsearch`, `movie`, `music`; `apikey`) |
| `GET /api/nzb/{ticket}` | the NZB for a result (`t=get&id=` answers the same) |
| `GET/POST /sab/api` | SABnzbd-compatible API |
| `GET /health` | version, ticket and job counts, real-SABnzbd reachability |

Configuration by environment variables:

| variable | meaning |
|---|---|
| `EASYNEWS_USERNAME`, `EASYNEWS_PASSWORD` | the account |
| `API_KEY` | accepted by both endpoints; when fronting a real SABnzbd, use that SABnzbd's key so the apps' existing configuration stays valid |
| `PUBLIC_URL` | how the apps reach this service, e.g. `http://192.0.2.10:8090`; used in result links |
| `SAB_URL`, `SAB_API_KEY` | optional real SABnzbd to front (`http://sabnzbd:8080`) |
| `INCOMPLETE_DIR`, `COMPLETE_DIR` | where downloads go; `COMPLETE_DIR/<category>/<release>/` is what the apps import from and must be the same path in every container |
| `STATE_FILE` | JSON state (tickets, jobs), default `/config/state.json`, rewritten atomically |

Run as the same uid as the owner of the download folders. Tickets live 48 hours.

## 3. Newznab side

### 3.1 caps

Standard caps XML: `<server>`, `<limits max="100" default="50"/>`, `<searching>` with `search`,
`tv-search` (`q,season,ep`), `movie-search` (`q,year`), `music-search` (`q,artist,album`),
`book-search` unavailable, and a `<categories>` tree: 2000 Movies (2030 SD, 2040 HD, 2045 UHD),
5000 TV (5030 SD, 5040 HD, 5045 UHD), 3000 Audio (3010 MP3, 3040 Lossless). Prowlarr caches caps
per indexer; after a change, edit and save the indexer to refresh.

### 3.2 Query mapping

| Newznab call | Easynews query | type filter | post-filter |
|---|---|---|---|
| `t=search&q=` | `q` | none | none |
| `t=movie&q=&year=` | `q year` | VIDEO | size >= 300 MB; name contains the year when given |
| `t=tvsearch&q=&season=&ep=` | `q S{season:02d}E{ep:02d}`, then `q {season}x{ep:02d}` if empty | VIDEO | name matches the episode pattern; size >= 100 MB; season packs out of scope |
| `t=music&artist=&album=` (or `q`) | `artist album` | AUDIO | group by `setid` into one release per set |

Drop items with `passwd` or `virus` true, sizes under 5 MB, names containing `sample`, and IMAGE or
OTHER types. Honour `cat` (comma list), `limit` (page size) and `offset` (page).

### 3.3 Releases and tickets

- **Video**: one release per file. Title = `fn`, size = `rawSize`, category from `fullres` (height
  >= 2000 -> 2045/5045, >= 700 -> 2040/5040, else 2030/5030) and the search mode (movie -> 2000s,
  tv -> 5000s; plain search: an `SxxEyy` in the name means TV, else movie).
- **Audio**: group by `setid` (fallback: poster plus the file-name prefix up to the last ` - `).
  One release per group: title = common prefix of the names with trailing separators and track
  numbers stripped, size = sum, category 3040 if any `.flac` else 3010, file list kept in the ticket.
  A group of one file is a single, which is a valid release.
- Skip groups whose file names share no letters with the query (obfuscated sets).
- Ticket token = first 24 hex chars of sha1 over the sorted file hashes. Stored:
  `{name, category, files:[{hash, extension, fn, size, sig}], created}`. `guid` and `link` are
  `{PUBLIC_URL}/api/nzb/{token}?apikey=...`.

### 3.4 RSS item shape

Namespace `xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"`. Per item: `title`,
`guid isPermaLink="true"`, `link`, `pubDate` (RFC 822 from `ts`), `category` text, `enclosure
url="..." length="<bytes>" type="application/x-nzb"`, and `newznab:attr` elements `category` (one
per category id), `size`, `guid`, `files`, `poster`, `group`, `usenetdate`, `grabs`.

## 4. The ticket NZB

Radarr, Sonarr and Prowlarr only check that a download is an NZB with at least one `<file>` holding a
`<segment>`. Serve this with `Content-Type: application/x-nzb` and a `Content-Disposition` file name:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="title">TITLE</meta>
    <meta type="x-easynews-ticket">TOKEN</meta>
  </head>
  <file poster="easynews-nzb" date="EPOCH" subject="TITLE">
    <groups><group>alt.binaries.misc</group></groups>
    <segments><segment bytes="BYTES" number="1">TOKEN@easynews-nzb</segment></segments>
  </file>
</nzb>
```

Keep `BYTES` equal to the release size (the apps compare loosely). XML-escape the title.

## 5. SABnzbd-compatible side

Copy the exact JSON shapes from a real SABnzbd (`mode=version`, `get_config`, `queue`,
`history`, `get_cats` with `output=json`). Behaviour:

| call | behaviour |
|---|---|
| `mode=version` | the fronted SABnzbd's version, or `5.1.3` standalone |
| `mode=get_config`, `mode=get_cats` | forward when fronting (cache the last good answer and serve it if the real SABnzbd is down); standalone: a minimal config with `misc.complete_dir` and the categories |
| `mode=addfile` (multipart, file field `name` or `nzbfile`, plus `cat`, `priority`, `nzbname`, `pp`) | parse the XML; ticket present -> create a job, answer `{"status":true,"nzo_ids":["ez_<token>"]}`; otherwise forward the multipart request unchanged |
| `mode=addurl` | forward |
| `mode=queue` | real queue plus own active jobs as slots (`nzo_id`, `filename`, `cat`, `status` Downloading/Queued, `percentage`, `mb`, `mbleft`, `sizeleft`, `timeleft` HH:MM:SS, `priority`, `index`); honour `nzo_ids`, `start`, `limit` |
| `mode=history` | same merge for finished jobs: `nzo_id`, `name`, `nzb_name`, `category`, `status` Completed/Failed, `storage` (completed folder path), `bytes`, `size`, `completed` (epoch), `download_time`, `postproc_time`, `fail_message`, `stage_log` `[]`, `url`; honour `category`, `start`, `limit` |
| `mode=queue&name=delete`, `mode=history&name=delete` (`value`, `del_files`) | `ez_` ids handled locally (delete files when asked), others forwarded |
| everything else | forwarded verbatim (pause, resume, status ...) |

Forwarding sends the same method, query string and body to `SAB_URL/api`.

## 6. Download jobs

- Job = ticket + category + nzbname. Folder = sanitised title (no `/`, no leading dot).
- Every file to `INCOMPLETE_DIR/<folder>/<fn><ext>` by streaming with `Range` resume, two at a
  time; then move the folder to `COMPLETE_DIR/<category>/<folder>/`, mark Completed with `storage`
  set to that path. Track `bytes_done` per file for the queue slot.
- `401`/`403` from Easynews: re-run the search once for a fresh `sig`; any other failure marks the
  job Failed with a `fail_message` so the app tries the next release.
- asyncio task queue in-process; state persisted so a restart re-queues jobs that were downloading
  (they resume from the partial file).
- Directories 2775, files 664 (umask 002).

## 7. Verification, standalone

With `KEY` = the configured API key and the service on `127.0.0.1:8090`:

1. `t=caps` returns the XML.
2. `t=movie&q=<film>&year=<year>` returns items with sizes and `application/x-nzb` enclosures.
3. `t=music&artist=<artist>&album=<album>` returns at least one release.
4. Fetching a `link` returns the ticket NZB.
5. `/sab/api?mode=version`, `get_config`, `queue`, `history` match SABnzbd's shapes.
6. Posting the NZB with `mode=addfile` and `cat=music` answers an `ez_` id; the job appears in
   `queue`, then in `history` as Completed; the files sit under `COMPLETE_DIR/music/<folder>/`;
   `history&name=delete&del_files=1` removes them.
7. When fronting: posting a genuine NZB must land in the real SABnzbd and appear in the merged history.

Then in the apps: Prowlarr "Generic Newznab" with `PUBLIC_URL`, API path `/api`, the key, Movies +
TV + Audio categories; the apps' SABnzbd client pointed at the service with URL base `/sab`; one
film, one episode and one album grabbed through Easynews and imported; one grab from another indexer
still going through the real SABnzbd.

## 8. Gotchas

- Run as the owner of the download folders; a root that maps to nobody on the host cannot write there.
- `storage` in history is what the apps import from; it must be the path as they see it.
- Prowlarr's indexer test performs a real search; the search must already return results.
- Obfuscated posts: show and match on `fn`, not `subject`.
- Junk thresholds: films under 300 MB, episodes under 100 MB, audio files under 5 MB.
- If the real SABnzbd is down, serve the cached `get_config` so Easynews jobs keep flowing.
