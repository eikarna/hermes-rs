with open("crates/hermes-core/src/tools/file_tools.rs", "r") as f:
    content = f.read()

import re

# Match the first conflict marker section
pattern1 = r'<<<<<<< HEAD\n(.*?)\n=======\n(.*?)\n>>>>>>> origin/main'
matches = list(re.finditer(pattern1, content, re.DOTALL))

print(f"Found {len(matches)} conflicts")
