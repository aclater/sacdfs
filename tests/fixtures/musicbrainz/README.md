# Recorded MusicBrainz responses

Responses of `https://musicbrainz.org/ws/2`, recorded on 2026-11-05 at the rate limit, for the
tests of `sacdfs tags fetch` (`tests/tags_fetch.rs`), so that CI never touches the network.

The structure, the track lengths and the media formats are as recorded. Everything that names
something is replaced, consistently: titles and names (`Title 5`, `Name 2`, the same original
gives the same text), ids, barcodes, catalogue numbers and ISRCs. Fields a lookup doesn't read are
dropped.

| File | Recorded from |
|---|---|
| `search-barcode.json` | `release?query=barcode:...`: a hybrid SACD and a digital release with one barcode |
| `search-catno.json` | `release?query=catno:"..."`: the same two |
| `search-empty.json` | a search that finds nothing |
| `discid-fuzzy.json` | `discid/-?toc=...&media-format=all`: those two, and a box set holding the same disc, as an SACD and as a digital release |
| `release-hybrid.json` | `release/ID?inc=...`: the hybrid SACD |
| `release-digital-twin.json` | the digital release |
| `release-box.json` | the box set as an SACD: three media, the same disc as its third, another album title |
