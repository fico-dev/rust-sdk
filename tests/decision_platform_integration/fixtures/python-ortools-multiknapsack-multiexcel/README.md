# Python OR-Tools Multi-Knapsack Multi-Excel

The workspace that the multi-file integration suite (`tests/decision_platform_integration`)
pushes and runs. It solves a multi-knapsack problem with OR-Tools: it reads
`inputs/input.xlsx` and writes `outputs/solutions/assignments.xlsx`,
`outputs/solutions/assignments.csv` and `outputs/metrics.json`.

It is nextmv's [`python-ortools-multiknapsack-multiexcel`][source] community
app, at commit `79b2fe0`, under the Apache License 2.0 in `LICENSE`. Changes:

* `app.yaml` is `workspace.yaml`, reworded for a workspace.
* `main.py` declares its options and the multi-file format, and passes them
  to `nextmv.load` and `nextmv.write`, because the nextmv library reads them
  from a manifest only when it is named `app.yaml`. Without them, it reads
  and writes JSON on stdin and stdout.
* `requirements.txt`, which repeats `pyproject.toml`, and `uv.lock` are left
  out.

Run it locally:

```bash
uv sync
uv run main.py -duration 30 -provider SCIP
```

[source]: https://github.com/nextmv-io/community-apps/tree/79b2fe0d998c582f345ebd344498996f0d337da7/python-ortools-multiknapsack-multiexcel
