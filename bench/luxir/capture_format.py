"""Generate rustfmt artifacts; the agent applies them through NUTS."""
from pathlib import Path
import subprocess
import sys

for filename in sys.argv[1:] or ["bm25.rs"]:
    if filename not in ("bm25.rs", "search.rs"):
        raise ValueError("unexpected source file")
    source = Path("/workspace/lume/src") / filename
    result = subprocess.run(["rustfmt", "--edition", "2021", "--emit", "stdout", str(source)],
                            check=True, capture_output=True, text=True)
    text = result.stdout
    header = str(source) + ":\n\n"
    if text.startswith(header):
        text = text[len(header):]
    destination = Path("/bench/hot-path") / (source.stem + ".formatted.rs")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text)
    print("Generated formatter artifact:", destination)
