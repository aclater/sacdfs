# Cosmos Cloud servapp

`servapps/sacdfs/` is the app in the layout of the Cosmos servapps store
(`azukaar/cosmos-servapps-official`): `cosmos-compose.json` (the template and its install
form), `description.json`, `icon.png` and `screenshots/`. The `cosmos-icon` label points at the
store's own address, as the store's validator requires.

Until the app is in that store, add this folder as a market source: in Cosmos, Market,
Sources, Add Source, with the URL of `index.json` here (raw file on GitHub). `make-index.py`
writes `index.json` from the app folder; give it the branch name.

What was checked, and where: [docs/install/store-apps.md](../../docs/install/store-apps.md).
