# Decision workspace workflow

A **workspace** holds a decision model, such as an optimization model, and
everything that runs it on the FICO® platform: its **builds** (pushed
versions of its code), its **environments** (a build and its configuration,
e.g. `production` and `staging`), its **runs** (an environment's build on an
input), and its **tests**. On this machine, a workspace is a directory with
a `workspace.yaml` manifest, and its local runs live in `.fico/runs/` in it.

## Rules

- **Solver**: ask the user which solver or modeling library to use (e.g.
  Pyomo, OR-Tools, HiGHS, SimPy, Gurobi, Xpress, CPLEX, AMPL, Hexaly,
  scikit-learn, Timefold, CVXPY and others).
- **Metrics**: every workspace reports metrics in its output (`metrics`).
  - **Optimization** (MIP, LP, CP, heuristics): `status` (`optimal` |
    `suboptimal` | `infeasible` | `unbounded`) and `result_value` (the
    objective value).
  - **ML classifiers**: `accuracy`, `precision`, `recall`, `f1_score`,
    `roc_auc`, `log_loss` and the confusion matrix counts, not
    `status`/`result_value`.
  - **ML regression and forecasting**: `rmse`, `mae`, `r2` and domain
    metrics, not `status`/`result_value`.
  - **Simulation and rules engines**: domain metrics, such as throughput,
    utilization or queue length.
- **Visuals**: every workspace includes at least one Plotly visual as an
  asset.
- **Options**: every workspace exposes its user-facing options in
  `workspace.yaml`. A workspace that uses a random seed exposes a
  `random_seed` option (`option_type: int`, not required, no default).
- **Never** delete, promote or overwrite anything on the platform without the
  user's confirmation.

---

## File structure

```
my-workspace/
├── workspace.yaml    # The manifest (required)
├── main.py           # Entry point
├── visuals.py        # Plotly visuals
├── pyproject.toml    # Pinned Python dependencies (or requirements.txt)
├── input.json        # Default sample input
├── inputs/           # 3-5 inputs of varying size or scenario
│   ├── small.json
│   ├── medium.json
│   └── large.json
└── README.md         # Short description and usage
```

---

## Step 1: Create the workspace

Create the directory, then write a sample `workspace.yaml` into it with the
`decision_manifest_init` operation:

```
fico_operation_destructive(name="decision_manifest_init",
                           arguments={"workspace_dir": "my-workspace", "manifest_type": "python",
                                      "content_format": "json"})
```

It never replaces a `workspace.yaml` unless `overwrite` is set. The run tools
register the workspace on this machine the first time they run it. Then edit
`workspace.yaml` to add the workspace's own options.

---

## Step 2: Write `workspace.yaml`

### JSON format (default)

The input is one JSON document on stdin, and the output one JSON document on
stdout.

```yaml
type: python
runtime: ghcr.io/nextmv-io/runtime/python:3.11
python:
  pip-requirements: pyproject.toml # Can be a requirements.txt

files:
  - main.py
  - visuals.py

configuration:
  content:
    format: json
  options:
    strict: false
    items:
      - name: duration
        option_type: string
        default: 30s
        description: How long the solver may run.
        required: false
        ui:
          control_type: input
          display_name: Duration
```

### Multi-file format

The input and output are directories of files. Use it when the solver reads
or writes its own files, e.g. CSV, Excel or text.

```yaml
type: python
runtime: ghcr.io/nextmv-io/runtime/python:3.11
python:
  pip-requirements: pyproject.toml

files:
  - main.py
  - visuals.py

configuration:
  content:
    format: multi-file
    multi-file:
      input:
        path: inputs
      output:
        solutions: outputs/solutions
        metrics: outputs/metrics.json
        assets: outputs/assets.json
  options:
    strict: true
    validation:
      enforce: all
    items:
      - name: my_option
        option_type: int
        default: 10
        required: true
        ui:
          control_type: slider
          display_name: My Option
```

### Random seed option

```yaml
- name: random_seed
  option_type: int
  required: false
  ui:
    control_type: input
    display_name: Random Seed (optional, for reproducibility)
```

### Option types

| `option_type` | Valid `control_type` values | Notes |
| --- | --- | --- |
| `string` | `input`, `select`, `multiselect` | Use `additional_attributes.values` for select |
| `int` | `input`, `slider` | Use `additional_attributes` for min/max/step |
| `float` | `input`, `slider` | Same as int |
| `bool` | `toggle` | No additional attributes needed |

After every change, check the manifest:

```
decision_manifest_validate(src="my-workspace")
```

---

## Step 3: Write `main.py`

Workspaces read options as command-line flags (`-duration 30s`), as the
manifest lists them. Declare each option with `argparse`, with the
manifest's name, type and default, and ignore the flags it does not declare:

```python
import argparse

parser = argparse.ArgumentParser(allow_abbrev=False)
parser.add_argument("-duration", "--duration", type=str, default="30s",
                    help="How long the solver may run.")
options, _ = parser.parse_known_args()
```

A `bool` option comes as text (`-verbose true`): parse it with
`type=lambda value: value.lower() == "true"`, not `type=bool`.

### JSON format: read and write

```python
import json
import sys

data = json.load(sys.stdin)

stdout = sys.stdout
sys.stdout = sys.stderr  # Solver logs go to stderr, not stdout.

# ... solver ...

print(json.dumps({
    "options": vars(options),
    "solution": {"assignments": [...]},
    "assets": [chart],
    "metrics": {"result_value": objective_value, "status": "optimal"},
}, indent=2), file=stdout)
```

### Multi-file format: read and write

Read the input files directly, not stdin, and write the output files to
disk.

```python
import csv, json, os

with open("inputs/orders.csv", newline="") as f:
    rows = list(csv.DictReader(f))

# ... solver ...

os.makedirs("outputs/solutions", exist_ok=True)
with open("outputs/solutions/results.csv", "w", newline="") as f:
    writer = csv.DictWriter(f, fieldnames=[...])
    writer.writeheader()
    writer.writerows(results)

with open("outputs/metrics.json", "w") as f:
    json.dump({"result_value": obj, "status": "optimal"}, f)

# assets.json: the top level MUST be {"assets": [...]}.
# In each asset: visual.schema and visual.type (NOT visual_schema/visual_type),
# and content stays a list (do not unwrap it with [0]).
with open("outputs/assets.json", "w") as f:
    json.dump({"assets": charts}, f)  # The assets of visuals.py.
```

### Logs

Write progress to stderr, e.g. with `print(..., file=sys.stderr)`. stdout is
the output of a JSON workspace.

---

## Step 4: Write `visuals.py`

```python
import json
import plotly.graph_objects as go


def build_chart(solution, input_data, label="Solution"):
    fig = go.Figure()
    fig.add_trace(go.Scatter(...))
    fig.update_layout(title=label)

    return {
        "name": label,
        "content_type": "json",
        "visual": {"schema": "plotly", "type": "custom-tab", "label": label},
        "content": [json.loads(fig.to_json())],
    }
```

---

## Step 5: Pin the dependencies

Pin every dependency in `pyproject.toml` (or `requirements.txt`), including
`plotly`.

---

## Step 6: Create input files

Add `input.json`, the default, and an `inputs/` directory with 3 to 5 files
that vary the problem's size, how tight its constraints are, or its
scenario.

---

## Step 7: Run on this machine

A local run runs the files the manifest lists, as they are: build a compiled
workspace (Go, Java) for this machine first, e.g. `go build -o main .`.
Pushing builds it for the platform with the manifest's `build` command, so
build it locally again after a push.

### Run

**JSON format:**

```
decision_local_run_create(src="my-workspace", input_file="my-workspace/input.json")
decision_local_run_create(src="my-workspace", input={...}, options={"duration": "10s"})
```

**Multi-file format:**

```
decision_local_run_create(src="my-workspace", input_dir="my-workspace/inputs")
```

The tool waits up to `wait_seconds` (60 by default) and returns the run's
status and metrics. Add its output, logs, input or visuals with `include`:
large ones come back as file paths to read.

### Inspect runs

```
decision_local_run_list(src="my-workspace")
decision_local_run_get(src="my-workspace", run_id="<local run ID>", include=["logs"])
decision_run_compare(run_ids=["<local run ID>", "<local run ID>"], src="my-workspace")
```

A failed run's logs say why it failed. Run it again with another input or
other options with `from_run_id`.

### Sync to the platform

Upload local runs to a platform workspace, as tracked runs that platform
tools can list, compare and test with:

```
decision_local_workspace_sync(src="my-workspace", workspace_id="<workspace ID>")
```

---

## Step 8: Write `README.md`

Include these sections: the workspace's name and a one-sentence description,
Approach, Options (a table), Input format, Output (solution, metrics,
visuals), Running locally (the tool calls) and Syncing to the platform.

---

## Step 9: Deploy to the platform

No single tool deploys a workspace: chain the operation tools. Find each one
and its arguments with `fico_operation_search`, then call it with the tool of
its risk level (`fico_operation_read`, `fico_operation_write` or
`fico_operation_destructive`).

### 1. Create the platform workspace (first time only)

```
fico_operation_write(name="decision_platform_workspace_create",
                     arguments={"workspace_id": "my-workspace", "name": "My Workspace"})
```

### 2. Push the code and create a build

```
fico_operation_destructive(name="decision_platform_workspace_push",
                           arguments={"workspace_id": "my-workspace", "src": "my-workspace"})
fico_operation_write(name="decision_platform_build_create",
                     arguments={"workspace_id": "my-workspace", "build_id": "v1", "name": "Build v1"})
```

A push replaces the workspace's code, and a build captures what was pushed.
Build and environment names need at least 3 characters, and names and
descriptions may not hold `+`, `=`, `@`, `"` or HTML tags.

### 3. Create the environments

```
fico_operation_write(name="decision_platform_environment_create",
                     arguments={"workspace_id": "my-workspace", "build_id": "v1",
                                "environment_id": "production", "name": "Production"})
fico_operation_write(name="decision_platform_environment_create",
                     arguments={"workspace_id": "my-workspace", "build_id": "v1",
                                "environment_id": "staging", "name": "Staging"})
```

Check what exists with `decision_workspace_get`.

### 4. Run every input on the platform

```
decision_run_create(workspace_id="my-workspace", environment_id="staging", input={...})
decision_run_create(workspace_id="my-workspace", environment_id="staging", input_file="my-workspace/inputs/large.json")
```

### 5. Create an input set

```
fico_operation_write(name="decision_platform_input_set_create",
                     arguments={"workspace_id": "my-workspace", "name": "Test inputs",
                                "environment_id": "staging", "maximum_runs": 10})
```

### 6. Run a scenario test

Compare option values or environments side by side:

```
decision_scenario_create(workspace_id="my-workspace", input_set_id="<input set ID>",
                         scenarios=[{"environment_id": "staging", "options": {"duration": ["5s", "30s"]}}])
```

### 7. Run an acceptance test

Check a candidate environment against a baseline:

```
decision_acceptance_create(workspace_id="my-workspace", candidate_environment_id="staging",
                           baseline_environment_id="production", input_set_id="<input set ID>",
                           metrics=[{"field": "result_value", "metric_type": "direct-comparison",
                                     "statistic": "mean",
                                     "params": {"operator": "le", "tolerance": {"type": "relative", "value": 5}}}])
```

A `relative` tolerance is a percentage: `5` is 5%. Get the results later
with `decision_get` and kind `acceptance`.

### 8. Promote (only with the user's confirmation)

**Always ask the user before you promote a build to production.** Push again,
create the new build, and point the environment at it:

```
fico_operation_destructive(name="decision_platform_environment_update",
                           arguments={"workspace_id": "my-workspace", "environment_id": "production",
                                      "build_id": "<new build ID>"})
```

---

## Other operations

Shadow and switchback tests, ensembles, managed inputs, secrets collections
and updates have no tool of their own: find them with `fico_operation_search`.
For example, to start a shadow test, search for `shadow create`, then call
`decision_platform_shadow_create` with `fico_operation_write`.

---

## Checklist

- [ ] `workspace.yaml` has the user-facing options, and
      `decision_manifest_validate` accepts it
- [ ] `main.py` declares the options it reads
- [ ] Metrics are in the output
- [ ] At least one Plotly visual as an asset
- [ ] Dependencies are Apache 2.0, MIT or BSD licensed, and pinned
- [ ] `input.json` and an `inputs/` directory with 3 to 5 varied inputs
- [ ] Local runs succeed with the expected metrics
- [ ] At least one non-default option tested
- [ ] Pushed, built, and the environments created
- [ ] An acceptance test of staging against production, reviewed
- [ ] **The user confirmed** before promoting to production
