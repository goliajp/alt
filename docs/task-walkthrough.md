# Run an installed task from start to Git exit

The package includes `examples/task-workflow.py`. It is an executable example,
not one of the real project acceptance tasks. It creates everything beneath one
new directory on local APFS or ext4, outside any existing repository.

After [installation](internal-install.md), choose an absolute Git executable:

```sh
python3 "$HOME/.local/alt-internal/current/examples/task-workflow.py" \
  --alt "$HOME/.local/alt-internal/bin/alt" \
  --git /absolute/path/to/git \
  --directory "$HOME/alt-example"
```

The directory must not exist. The example retains every command's argv, exit
code and exact stdout/stderr bytes in `commands.jsonl`, and writes `result.json`
only after the complete flow passes. It neither installs tools nor pushes or
deploys the result. The retained main workspace remains at its original content.

The script performs these explicit steps:

1. Create a native repository with `message.txt` and a simple gate, commit the
   baseline, then create a named workspace on a separate task branch.
2. Probe the actual Bash executable and declare the complete validation
   environment. Create a Task before editing, with only `message.txt` allowed.
3. Stage the exact `base` → `reviewed` change and capture a Candidate. Run its
   isolated snapshot and require eligible observed Evidence.
4. Read `task changes` and check the exact single-file change. Submit a Review
   naming the example's invoker and its explicit authorization.
5. Switch the main workspace to a holding branch, freeing the target. Integrate
   against the expected target, repeat the same request id, and verify the replay
   neither duplicates integration nor changes the main workspace's index.
6. Export the completed Integration into `git-exit`, collect its verified
   delivery, and independently run `git fsck --strict` and `git status`.

For a real task, supply its actual purpose, authority, allowed paths and full
validation inputs. Inspect the changes yourself before review. A task definition
is JSON passed to `--definition`, not the name of a JSON file. A deliberately
incomplete environment or unknown tool identity prevents eligible evidence.

Use `alt task --help`, `alt candidate --help`, `alt evidence --help` and
`alt runner --help` for the exact command surface. The [recovery guide](recovery.md)
explains unknown requests, failed validation, stale candidates, collection and
old-log refusals. Do not restart the example in its existing directory after a
failure; inspect its retained receipts first, then use a new directory when
starting a genuinely new run.
