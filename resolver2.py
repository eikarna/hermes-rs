with open("crates/hermes-core/src/tools/file_tools.rs", "r") as f:
    content = f.read()

import re
content = re.sub(r'<<<<<<< HEAD\n(.*?)\n=======\n(.*?)\n>>>>>>> origin/main', r'\2', content, flags=re.DOTALL)

with open("crates/hermes-core/src/tools/file_tools.rs", "w") as f:
    f.write(content)
print("Resolved remaining conflicts.")
