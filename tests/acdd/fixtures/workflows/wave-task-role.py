"""Provision only test protocol inputs in the owned Git worktree."""
from pathlib import Path
import runpy
import shutil
import subprocess
import sys

fixture, executable, session, prompt = sys.argv[1:]
root = Path.cwd()
common = Path(subprocess.check_output(["git", "rev-parse", "--git-common-dir"], text=True).strip())
primary = (root / common).resolve().parent
control = root / ".acdd/lifecycle"
control.mkdir(parents=True, exist_ok=True)
runner = root / ".acdd/runner_fixture"
if not runner.exists():
    shutil.copy2(primary / ".acdd/runner_fixture", runner)
if (primary / ".acdd/fail-isolated-role").exists():
    sys.exit(17)
sys.argv = [fixture, executable, session, prompt]
runpy.run_path(fixture, run_name="__main__")
