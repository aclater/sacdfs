# Fuzzing

cargo-fuzz targets for what sacdfs adds on top of scarletbook, whose own parsers are fuzzed in
its repository (#161). A crate of its own: it needs a nightly toolchain, and it isn't part of
sacdfs's package or its build.

| Target | Input | What must hold |
|---|---|---|
| `tags_toml` | a tags file (`--tags`) | No panic. Its text form reads back as the same tags. Applied through the tag store to a real disc's tracks, every tag is well formed |
| `sidecars` | a `--sidecars` list, a name, a link's target, a path | Hidden names and images never pass the list. Nothing outside SOURCE is read, and no target lies under MOUNTPOINT |
| `names` | track numbers, titles and names in SOURCE | Every name is one path component of at most `NAME_MAX` bytes, unique in its directory |
| `disc_text` | an image from its master TOC on | From hostile text in the TOC: file names, DSF tag and header, WAV chunks and header are all well formed |
| `tree` | two image file names and an image | Every entry of the tree is a valid name, unique, found by lookup; every file's header and tag can be read |
| `headers` | a file shape and tag values | The ID3, DSF and WAV writers: tags and chunks are well formed for any text. A WAV header read in pieces is the header read whole |
| `verify_container` | one or two DSF or DFF files | The `verify` readers: the audio stream has the length the header says, and a search of one stream in another ends |
| `verify_id3` | one or two ID3v2 tags | The `verify` and `tags import` readers: no panic, and the report's form of what they find reads back |
| `lookup_json` | a MusicBrainz response (`sacdfs tags fetch`) | No panic, whatever is malformed. Matched against a real disc's areas, what would be written reads back as the same tags |
| `read_path` | a corpus image (plain DSD or DST, each sector size), damage to it, settings and reads | Every read of a DSF or WAV file, at any offset and length, on readers that stay open with their read-ahead and through small caches whose buffers are reused, returns the right length and the same bytes as a new reader with no cache (#390) |
| `verify_report` | a line of a `verify` report | Any bytes survive a field's escaping, and a line read is written back the same |

`verify` reads DSF and DFF files, not FLAC, so there is no FLAC target. The persistent seek
index, whose files live in the cache directory, is tested separately in `tests/index_cache.rs`.

Each target also runs what it needs of scarletbook, so a crash can be in the library. Don't work
around it: report it to scarletbook-rs as a requirement, describing what goes in and what comes
out.

## Running

You need a nightly toolchain and cargo-fuzz, and C++ for libFuzzer:

```sh
rustup toolchain install nightly --profile minimal
cargo +nightly install cargo-fuzz --locked
```

Then, with nightly as the default toolchain in this directory or with `RUSTUP_TOOLCHAIN=nightly`:

```sh
fuzz/run.sh 60 4                  # every target for 60 s, four at a time
fuzz/run.sh 600 2 tags_toml names # these two, for 10 minutes each
```

`fuzz/run.sh` makes the seeds first (`examples/fuzz_seeds.rs`, into `fuzz/corpus/`): the test
corpus's images, what sacdfs serves for them, and hand-made files of the kinds the readers tell
apart. No seed is checked in. It prints one line per target and exits non-zero if a target
crashed. A target's output is in `fuzz/logs/<target>.log`, and the input that crashed it in
`fuzz/artifacts/<target>/`. To run one again:

```sh
cargo fuzz run TARGET fuzz/artifacts/TARGET/crash-...
```

Temporary files go to `$TMPDIR`, in `sacdfs-fuzz-*`, and are left behind when a target ends:
libFuzzer leaves no time to remove them.

## When a target crashes

1. Fix it, in sacdfs if the bug is there.
2. Add the smallest input that shows it to `tests/fuzz_regressions.rs`.
3. A crash of the mount from an image, or a read outside SOURCE, is a vulnerability: follow
   [SECURITY.md](../SECURITY.md) and post no details until it is fixed. CI runs and their
   artifacts are public, so don't re-run a known vulnerability's input there either.

## In CI

`.github/workflows/fuzz.yml` runs every target for about 60 s on pull requests and pushes to
main, four at a time, and for 20 minutes each on Tuesdays, starting from the corpus the last
weekly run left. A failed run keeps the crashing inputs as an artifact for 30 days. It is not a
required check.
