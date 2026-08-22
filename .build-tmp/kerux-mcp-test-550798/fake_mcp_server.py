
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {
            "protocol_version": "2024-11-05",
            "capabilities": {"tools": {}},
            "server_info": {"name": "fake-stdio", "version": "1.0.0"},
        }
    elif method == "tools/list":
        result = {
            "tools": [{
                "name": "stdio_echo",
                "description": "Echoes text over stdio",
                "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}},
            }]
        }
    elif method == "tools/call":
        text = request.get("params", {}).get("arguments", {}).get("text", "")
        result = {"content": [{"type": "text", "text": text}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request.get("id"), "result": result}), flush=True)
