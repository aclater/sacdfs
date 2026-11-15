#!/usr/bin/python3 -I
# Writes index.json, a Cosmos market source for this folder, as the store's index.js would,
# but with raw GitHub URLs of this branch. Usage: make-index.py [BRANCH]
import json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
base = "https://raw.githubusercontent.com/aclater/sacdfs/%s/deploy/cosmos" % (sys.argv[1] if len(sys.argv) > 1 else "main")
apps = []
for app in sorted(os.listdir(os.path.join(here, "servapps"))):
    d = os.path.join(here, "servapps", app)
    s = json.load(open(os.path.join(d, "description.json")))
    s["id"] = app
    s["screenshots"] = ["%s/servapps/%s/screenshots/%s" % (base, app, f) for f in sorted(os.listdir(os.path.join(d, "screenshots"))) if f.endswith(".png")]
    s["artefacts"] = {}
    s["icon"] = "%s/servapps/%s/icon.png" % (base, app)
    s["compose"] = "%s/servapps/%s/cosmos-compose.json" % (base, app)
    apps.append(s)
json.dump({"source": base + "/index.json", "showcase": [], "all": apps}, open(os.path.join(here, "index.json"), "w"), indent=2)
