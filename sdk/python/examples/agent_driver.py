"""Disposable SDK acceptance client; never launched by the Rust backend."""
import json
import sys
from kageha_client import AgentClient, AgentError
client = AgentClient(sys.argv[1], open(sys.argv[2], encoding="utf8").read().rstrip())
method = sys.argv[3]
value = json.load(sys.stdin)
try:
    if method == "run": result = client.run(value)
    elif method == "info": result = client.info()
    elif method == "status": result = client.status(value["run"])
    elif method == "cancel": result = client.cancel(value["run"])
    elif method == "steer": result = client.steer(value["run"], value["message"])
    elif method == "approvals": result = client.approvals(**value)
    elif method == "approval": result = client.approval(value["id"])
    elif method == "decide": result = client.decide(value["id"], value["decision"])
    elif method == "events":
        result = []
        with client.events(value["run"], after=value.get("after", 0)) as events:
            for event in events:
                result.append(event)
                if len(result) >= value.get("limit", 1024): break
    else: raise ValueError("unknown driver method")
    print(json.dumps({"value": result}))
except AgentError as error:
    print(json.dumps({"error": {"status": error.status, "code": error.code, "message": str(error), "retryable": error.retryable}}))
