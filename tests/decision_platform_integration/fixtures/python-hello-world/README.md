# Python Hello World

The workspace that the JSON integration suite (`tests/decision_platform_integration`) pushes and
runs. It reads a name, a radius and a distance as JSON from stdin, and writes a
message, a metric and a Plotly chart as JSON to stdout.

It is nextmv's [`python-hello-world`][source] community app, at commit
`79b2fe0`, under the Apache License 2.0 in `LICENSE`. Changes:

* `app.yaml` is `workspace.yaml`, reworded for a workspace.
* `main.py` declares its options and passes them to `nextmv.load`, because
  the nextmv library reads options from a manifest only when it is named
  `app.yaml`.
* `requirements.txt`, which repeats `pyproject.toml`, and `uv.lock` are left
  out.

Run it locally:

```bash
uv sync
cat input.json | uv run main.py -details true
```

[source]: https://github.com/nextmv-io/community-apps/tree/79b2fe0d998c582f345ebd344498996f0d337da7/python-hello-world
