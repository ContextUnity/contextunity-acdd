from pathlib import Path
import sys

Path("validated-arguments").write_text(" ".join(sys.argv[1:]))
