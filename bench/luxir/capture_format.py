"""Generate a rustfmt artifact; the agent applies it through NUTS."""
from pathlib import Path
import subprocess

source = Path("/workspace/lume/src/bm25.rs")
result = subprocess.run(["rustfmt", "--edition", "2021", "--emit", "stdout", str(source)],
                        check=True, capture_output=True, text=True)
text = result.stdout
header = str(source) + ":\n\n"
if text.startswith(header):
    text = text[len(header):]
destination = Path("/bench/hot-path/bm25.formatted.rs")
destination.parent.mkdir(parents=True, exist_ok=True)
destination.write_text(text)
print("Generated formatter artifact:", destination)
